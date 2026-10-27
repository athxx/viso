//! Microbench for the `ColorPicker` control: the cost of one authoring pass
//! (`ColorPicker::build`), one `layout`, one `paint_tree` lowering, and — the
//! interactive hot path — one colour change.
//!
//! - `color_picker/build`, `/layout`, `/paint_tree` exercise the real `build` ->
//!   `layout` -> `paint_tree` path over the default picker: the tone square, the
//!   hue and opacity bars, the hex field and the swatch.
//! - `color_picker/change_frame` times what one arrow-key nudge on the focused
//!   tone square costs: the key routes to its handler, which steps the
//!   saturation, writes the colour cell and fires `on_change`; the state flush
//!   then runs the picker's structure hook, which shows the colour on every part
//!   (plane, swatch, both bars, their semantics) and reseeds the hex field, and
//!   the text reconcile re-declares the hex text. Right and Left alternate so
//!   the colour changes every frame without clamping. Relayout and repaint are
//!   measured by the phases above, not here.
//!
//! `ColorPicker` authors reactive cells and a structure hook, so it must build
//! through `BuildCx::with_reactive`. It drives only `viso-ui`, so it needs no
//! facade or render dev-dependency and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets --bench color_picker_build`);
//! debug timing is not a perf result.

use std::cell::Cell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::text_edit::reconcile;
use viso_ui::{
    BindingTable, BuildCx, Component, EffectStore, Key, KeyEvent, KeyRouter, Modifiers, NodeId,
    NodeStore, Rect, Rgba, Role, SemanticProjector, StateStore, TextEdits, VirtualLists,
    focus_node, paint_tree, run_structure_hooks,
};
use viso_widgets::{ColorPicker, ColorPickerLabels, color_picker};

const W: f32 = 232.0;
const H: f32 = 320.0;

/// The default picker at a mid-saturation blue, so a nudge either way moves it.
fn fixture(changes: Rc<Cell<u32>>) -> ColorPicker {
    color_picker("Fill")
        .value(Rgba::new(0.05, 0.2, 0.7, 1.0))
        .on_change(move |_, _| changes.set(changes.get() + 1))
}

/// A built picker with the stores a frame drives it through, so a bench can
/// route keys and settle the change the way the window driver does.
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
    changed: Vec<viso_ui::StateId>,
}

impl Frame {
    fn new(picker: ColorPicker) -> Self {
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
            picker.build(&mut cx);
            cx.root().expect("color picker declares a root")
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
            changed: Vec::new(),
        };
        // The first frame registers the hex field's buffer from its seed.
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

    /// Flush the state written and run the structure hooks it reaches, then
    /// fold the text seeds they queued into the hex field.
    fn settle(&mut self) {
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
        reconcile(&mut self.store, &mut self.text_edits, None);
    }

    /// The live node whose semantics take `role` and are named `label`.
    fn find(&self, role: Role, label: &str) -> NodeId {
        (0..self.store.arena().slot_count() as u32)
            .filter_map(|i| self.store.arena().live_id(i))
            .find(|&id| {
                self.store
                    .semantics(id)
                    .is_some_and(|s| s.role == role && s.label.as_deref() == Some(label))
            })
            .unwrap_or_else(|| panic!("no {role:?} named {label}"))
    }

    /// The text the hex field's buffer holds.
    fn hex(&self) -> String {
        let hex = self.find(Role::TextField, &ColorPickerLabels::default().hex);
        self.text_edits
            .get(hex)
            .map(|buffer| buffer.text.clone())
            .unwrap_or_default()
    }
}

/// Author a picker into a fresh store and return the store plus its root — the
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

/// A picker with its tone square focused, ready for nudges, with its change
/// counter.
fn focused_frame() -> (Frame, Rc<Cell<u32>>) {
    let changes = Rc::new(Cell::new(0));
    let mut frame = Frame::new(fixture(Rc::clone(&changes)));
    let tone = frame.find(Role::Slider, &ColorPickerLabels::default().tone);
    assert!(focus_node(&mut frame.store, tone), "tone square focuses");
    frame.settle();
    (frame, changes)
}

/// Check a nudge changes the colour, fires `on_change` and reseeds the hex
/// field, and say what is measured.
fn describe() {
    let (mut frame, changes) = focused_frame();
    let nodes = frame.store.arena().live_count();
    let before = frame.hex();
    frame.key(Key::Right);
    let after = frame.hex();
    assert_ne!(before, after, "a nudge shows the new colour as hex");
    frame.key(Key::Left);
    assert_eq!(changes.get(), 2, "each nudge fires on_change once");
    frame.store.layout(frame.root, surface(), &mut Vec::new());
    let mut primitives = Vec::new();
    paint_tree(&frame.store, frame.root, &mut primitives);
    assert!(!primitives.is_empty(), "a laid-out picker paints");
    eprintln!(
        "color_picker: square + hue/opacity bars + hex + swatch ({nodes} nodes, {} primitives); \
         change_frame = arrow key -> colour cell + on_change -> structure hook shows every \
         part -> hex reseed ({before} -> {after}; no relayout)",
        primitives.len()
    );
}

fn bench_color_picker(c: &mut Criterion) {
    describe();

    // build: author the picker into a fresh NodeStore through a reactive cx.
    c.bench_function("color_picker/build", |b| {
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

    // layout: lay the built picker out into the surface rect.
    c.bench_function("color_picker/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out picker into the reused primitive buffer.
    c.bench_function("color_picker/paint_tree", |b| {
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

    // change_frame: one arrow-key nudge on the focused tone square, settled
    // through the structure hook and the hex reseed. Right and Left alternate
    // so every frame changes the colour.
    c.bench_function("color_picker/change_frame", |b| {
        let (mut frame, changes) = focused_frame();
        let mut right = true;
        b.iter(|| {
            frame.key(if right { Key::Right } else { Key::Left });
            right = !right;
            black_box(&frame.store);
        });
        assert!(changes.get() > 0, "the timed nudges fired on_change");
    });
}

criterion_group!(benches, bench_color_picker);
criterion_main!(benches);
