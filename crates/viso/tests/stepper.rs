//! A view's `Stepper`: a press on a half steps its number and holding it
//! steps on, faster the longer it is held, stopping at a bound; the keys
//! step it; it reports its number to assistive technology as a spin button.

use std::time::Duration;

use viso::render::Rect;
use viso::ui::{
    BindingTable, BuildCx, EffectStore, Instant, Key, KeyEvent, KeyRouter, NodeId, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, PointerRouter, Role, SemanticProjector, StateStore,
    TextEdits, TimerRegistry, VirtualLists, focus_node, run_structure_hooks,
};

viso::component! {
    Guests {
        state guests: F64 = 0.0;
        view {
            Column {
                width: 300dp;
                height: 200dp;
                Stepper { bind value <=> guests; min: 0.0; max: 3.0; label: "Guests"; }
            }
        }
    }
}

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
    timers: TimerRegistry,
    now: Instant,
    root: NodeId,
}

impl Mounted {
    fn new() -> Self {
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
            Guests::build(&mut cx).1.id()
        };
        store.layout(root, SURFACE, &mut Vec::new());
        Self {
            store,
            states,
            bindings,
            timers: TimerRegistry::new(),
            now: Instant::now(),
            root,
        }
    }

    fn stepper(&self) -> NodeId {
        self.store.arena().first_child(self.root).unwrap()
    }

    fn guests(&self) -> f32 {
        let state = self.store.semantic_state(self.stepper()).unwrap();
        state.value.unwrap()
    }

    /// Run the flush, then arm the timers the frame asked for.
    fn flush(&mut self) {
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
        let mut requests = Vec::new();
        self.store.take_timer_requests(&mut requests);
        for request in requests {
            self.timers.arm_request(request, self.now);
        }
    }

    fn pointer(&mut self, x: f32, y: f32, phase: PointerPhase) {
        let event = PointerEvent {
            x,
            y,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Default::default(),
        };
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.root,
            event,
            &mut Vec::new(),
        );
        self.flush();
    }

    /// Let `d` pass: fire the timers due, deliver their wakes.
    fn wait(&mut self, d: Duration) {
        self.now += d;
        self.timers.fire_due(&mut self.store, self.now);
        KeyRouter::route_wakes(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut TextEdits::new(),
        );
        self.flush();
    }

    fn key(&mut self, key: Key) {
        let stepper = self.stepper();
        focus_node(&mut self.store, stepper);
        let ev = KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut TextEdits::new(),
            self.root,
            ev,
            &mut Vec::new(),
        );
        self.flush();
    }
}

#[test]
fn a_held_plus_steps_on_until_its_bound_and_the_minus_dims_at_the_least() {
    let mut view = Mounted::new();
    let stepper = view.stepper();
    let at = view.store.bounds(stepper);
    assert_eq!((at.w, at.h), (96.0, 32.0));
    let semantics = view.store.semantics(stepper).unwrap();
    assert_eq!(semantics.role, Role::SpinButton);
    assert_eq!(semantics.label.as_deref(), Some("Guests"));
    let pill = view.store.stepper(stepper).unwrap();
    assert!(!pill.lower && pill.upper, "at its least, the minus dims");

    let plus = (at.x + at.w * 0.75, at.y + at.h * 0.5);
    view.pointer(plus.0, plus.1, PointerPhase::Down);
    assert_eq!(view.guests(), 1.0, "a press steps at once");
    view.wait(Duration::from_millis(300));
    assert_eq!(view.guests(), 1.0, "and holds before stepping on");
    view.wait(Duration::from_millis(250));
    assert_eq!(view.guests(), 2.0);
    view.wait(Duration::from_millis(130));
    assert_eq!(view.guests(), 3.0);
    view.wait(Duration::from_millis(500));
    assert_eq!(view.guests(), 3.0, "it stops at its most");
    assert!(!view.store.stepper(stepper).unwrap().upper);
    view.pointer(plus.0, plus.1, PointerPhase::Up);
    assert_eq!(view.store.stepper(stepper).unwrap().held, None);
}

#[test]
fn keys_step_it_and_take_it_to_its_bounds() {
    let mut view = Mounted::new();
    view.key(Key::Up);
    view.key(Key::Up);
    assert_eq!(view.guests(), 2.0);
    view.key(Key::Down);
    assert_eq!(view.guests(), 1.0);
    view.key(Key::End);
    assert_eq!(view.guests(), 3.0);
    view.key(Key::PageDown);
    assert_eq!(view.guests(), 0.0);
}

#[test]
fn a_stepper_draws_its_pill_halves_and_dims_a_spent_one_and_matches_golden() {
    use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle};
    use viso::render::{Renderer, Rgba};
    use viso::ui::{Axis, BoxStyle, Component, Inset, Sizing, paint_tree};
    use viso::widgets::{ViewStyle, stepper, view};

    const W: u32 = 120;
    const H: u32 = 84;
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
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
        view(ViewStyle {
            axis: Axis::Column,
            padding: Inset::all(8.0),
            gap: 4.0,
            size: Sizing::fill(),
            background: BoxStyle::solid(Rgba::new(0.1, 0.1, 0.12, 1.0)),
            ..Default::default()
        })
        .children(|cx| {
            stepper(0.0).range(0.0, 3.0).build(cx);
            stepper(1.0).range(0.0, 3.0).build(cx);
        })
        .build(&mut cx);
        cx.root().unwrap()
    };
    store.layout(
        root,
        Rect::new(0.0, 0.0, W as f32, H as f32),
        &mut Vec::new(),
    );
    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    let actual = gpu.read_pixels_bgra8(surface);
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/stepper.bgra8");
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read(&path)
        .unwrap_or_else(|_| panic!("missing golden {}; run with BLESS=1", path.display()));
    let worst = actual
        .iter()
        .zip(&expected)
        .map(|(a, e)| a.abs_diff(*e))
        .max()
        .unwrap_or(0);
    assert!(worst <= 2, "golden mismatch: max per-channel diff {worst}");
}
