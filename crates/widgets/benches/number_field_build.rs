//! Microbench for the `NumberField` control: the cost of one authoring pass
//! (`NumberField::build`), one `layout`, one `paint_tree` lowering, and — the
//! interactive hot path — one arrow-key step frame.
//!
//! - `number_field/build`, `/layout`, `/paint_tree` exercise the real `build` ->
//!   `layout` -> `paint_tree` path over one field, its text seeded.
//! - `number_field/step_frame` times what one Up/Down press costs the field once
//!   it has the focus: the key routes to its handler (which queues a step
//!   intent), the text reconcile folds the step into the buffer and re-declares
//!   the text, the change is delivered back to the handler (which reads the new
//!   number, writes the announced value cell and fires `on_change`), and the
//!   frame's state flush and structure hooks run. Presses alternate Up and Down
//!   so the number moves every frame without reaching a bound. Relayout and
//!   repaint are measured by the phases above, not here.
//!
//! `NumberField` authors a reactive cell (`cx.state`) and a text field, so it
//! must build through `BuildCx::with_reactive`. It drives only `viso-ui`, so it
//! needs no facade or render dev-dependency and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets --bench number_field_build`);
//! debug timing is not a perf result.

use std::cell::Cell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::text_edit::reconcile;
use viso_ui::{
    BindingTable, BuildCx, Component, EffectStore, Key, KeyEvent, KeyRouter, Modifiers, NodeId,
    NodeStore, Rect, SemanticProjector, StateStore, TextEdits, VirtualLists, focus_node,
    paint_tree, run_structure_hooks,
};
use viso_widgets::{NumberField, number_field};

const W: f32 = 200.0;
const H: f32 = 32.0;

/// A field over a range wide enough that alternating steps never clamp.
fn fixture(changes: Rc<Cell<u32>>) -> NumberField {
    number_field("Volume")
        .range(0.0, 100.0)
        .step(0.5)
        .value(42.0)
        .on_change(move |_, _| changes.set(changes.get() + 1))
}

/// A built field with the stores a frame drives it through, so a bench can
/// route keys and settle the edit the way the window driver does.
struct Frame {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    /// Kept alive beside the store; the frame reads no semantics.
    _projectors: SemanticProjector,
    root: NodeId,
    chain: Vec<NodeId>,
    edited: Vec<NodeId>,
    text: String,
    changed: Vec<viso_ui::StateId>,
}

impl Frame {
    fn new(field: NumberField) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            field.build(&mut cx);
            cx.root().expect("number field declares a root")
        };
        let mut frame = Frame {
            store,
            states,
            bindings,
            effects: EffectStore::default(),
            lists,
            text_edits,
            _projectors: projectors,
            root,
            chain: Vec::new(),
            edited: Vec::new(),
            text: String::new(),
            changed: Vec::new(),
        };
        // The first frame registers the field's buffer from its seed.
        frame.settle();
        frame
    }

    /// Route a press of `key` to the focused node, then settle the frame.
    fn key(&mut self, key: Key) {
        let ev = KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        };
        KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
            self.root,
            ev,
            &mut self.chain,
        );
        self.settle();
    }

    /// Fold queued edits into their buffers, tell each edited field its text,
    /// then flush the state written and run the structure hooks it reaches.
    fn settle(&mut self) {
        reconcile(&mut self.store, &mut self.text_edits, None);
        self.text_edits.take_changed(&mut self.edited);
        for i in 0..self.edited.len() {
            let node = self.edited[i];
            let Some(buffer) = self.text_edits.get(node) else {
                continue;
            };
            self.text.clear();
            self.text.push_str(&buffer.text);
            KeyRouter::route_text_change(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.text_edits,
                node,
                &self.text,
            );
        }
        self.states.take_pending(&mut self.changed);
        self.store
            .flush_state_transactions(&self.changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &mut self.lists,
            &self.changed,
        );
    }

    /// The text the field's buffer holds.
    fn shown(&self) -> &str {
        self.text_edits
            .get(self.root)
            .map_or("", |buffer| buffer.text.as_str())
    }
}

/// Author a field into a fresh store and return the store plus its root — the
/// input the layout/paint phases below run on.
fn build_scene() -> (NodeStore, NodeId) {
    let frame = Frame::new(fixture(Rc::default()));
    (frame.store, frame.root)
}

fn surface() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W,
        h: H,
    }
}

/// A focused field ready for steps, with its change counter.
fn focused_frame() -> (Frame, Rc<Cell<u32>>) {
    let changes = Rc::new(Cell::new(0));
    let mut frame = Frame::new(fixture(Rc::clone(&changes)));
    assert!(focus_node(&mut frame.store, frame.root), "field focuses");
    frame.settle();
    (frame, changes)
}

/// Check a step moves the number and fires `on_change`, and say what is
/// measured.
fn describe() {
    let (mut frame, changes) = focused_frame();
    assert_eq!(frame.shown(), "42.0", "the seed shows the value");
    frame.key(Key::Up);
    assert_eq!(frame.shown(), "42.5", "Up steps the text");
    frame.key(Key::Down);
    assert_eq!(frame.shown(), "42.0", "Down steps it back");
    assert_eq!(changes.get(), 2, "each step fires on_change once");
    eprintln!(
        "number_field: one field; step_frame = Up/Down key -> step intent -> text reconcile \
         -> change delivery -> on_change + state flush + structure hooks (no relayout)"
    );
}

fn bench_number_field(c: &mut Criterion) {
    describe();

    // build: author the field into a fresh NodeStore through a reactive cx.
    c.bench_function("number_field/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut states = StateStore::new();
            let mut bindings = BindingTable::new();
            let mut lists = VirtualLists::new();
            let mut text_edits = TextEdits::new();
            let mut projectors = SemanticProjector::new();
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            fixture(Rc::default()).build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built field out into the surface rect.
    c.bench_function("number_field/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out field into the reused primitive buffer.
    c.bench_function("number_field/paint_tree", |b| {
        let (mut store, root) = build_scene();
        store.layout(root, surface(), &mut Vec::new());
        let mut primitives = Vec::new();
        // Warm the buffer to steady capacity so the timed loop does not grow it.
        paint_tree(&store, root, &mut primitives);
        b.iter(|| {
            primitives.clear();
            paint_tree(&store, root, &mut primitives);
            black_box(&primitives);
        });
    });

    // step_frame: one arrow-key step on the focused field, settled through the
    // edit reconcile and change delivery. Up and Down alternate so every frame
    // changes the number.
    c.bench_function("number_field/step_frame", |b| {
        let (mut frame, changes) = focused_frame();
        let mut up = true;
        b.iter(|| {
            frame.key(if up { Key::Up } else { Key::Down });
            up = !up;
            black_box(&frame.store);
        });
        assert!(changes.get() > 0, "the timed steps fired on_change");
    });
}

criterion_group!(benches, bench_number_field);
criterion_main!(benches);
