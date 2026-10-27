//! The `Slider`, `Knob` and `RangeSlider` controls driven through the
//! facade: each draws one slider node — a track filled to its thumb, marked
//! at its steps, filled from an origin, a range between two thumbs, a knob
//! pointing at its value, a bar running up — matching a blessed golden; a
//! press on the track jumps the thumb, one on the thumb picks it up, a drag
//! carries it at once and Shift a tenth as far; the keys step it and its
//! thumb eases there; a range's thumbs do not pass each other and each is a
//! focusable slider of its own; a knob turns with a drag up; a warmed-up
//! frame allocates the same each time.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::slider::Thumb;
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, Component, EffectStore, Inset, Key, KeyEvent, KeyRouter,
    Length, Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Role, SemanticProjector, Sizing, StateStore, TextEdits, VirtualLists,
    focus_node, paint_tree, run_structure_hooks,
};
use viso::widgets::{ViewStyle, knob, range_slider, slider, view};

const W: u32 = 260;
const H: u32 = 200;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

fn surface_rect() -> Rect {
    Rect::new(0.0, 0.0, W as f32, H as f32)
}

/// A light column of sliders: one marked at its steps, one filling from its
/// centre, a range, then a row of a knob and a bar running up.
fn build_scene(store: &mut NodeStore) -> NodeId {
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let width = Sizing {
        width: Length::Fixed(200.0),
        height: Length::Fit,
    };
    let container = view(ViewStyle {
        axis: Axis::Column,
        padding: Inset::all(10.0),
        gap: 6.0,
        size: Sizing::fill(),
        background: BoxStyle::solid(Rgba::new(0.97, 0.97, 0.98, 1.0)),
        ..Default::default()
    })
    .children(move |cx| {
        slider("Steps")
            .range(0.0, 100.0)
            .step(10.0)
            .marks(true)
            .value(60.0)
            .size(width)
            .build(cx);
        slider("Balance")
            .range(-1.0, 1.0)
            .origin(0.0)
            .value(-0.5)
            .size(width)
            .build(cx);
        range_slider("Price")
            .range(0.0, 100.0)
            .values(20.0, 70.0)
            .size(Sizing {
                width: Length::Fixed(200.0),
                height: Length::Fixed(24.0),
            })
            .build(cx);
        view(ViewStyle {
            axis: Axis::Row,
            gap: 20.0,
            ..Default::default()
        })
        .children(|cx| {
            knob("Gain").value(0.75).build(cx);
            slider("Level")
                .vertical()
                .value(0.3)
                .size(Sizing {
                    width: Length::Fit,
                    height: Length::Fixed(80.0),
                })
                .build(cx);
        })
        .build(cx);
    });
    let mut cx = BuildCx::with_reactive(
        store,
        &mut states,
        &mut bindings,
        &mut lists,
        &mut text_edits,
        &mut projectors,
    );
    container.build(&mut cx);
    cx.root().expect("scene has a root")
}

#[test]
fn sliders_draw_their_tracks_thumbs_marks_and_knob_and_match_golden() {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    let mut store = NodeStore::new();
    // The scene is dark, so it draws in the dark theme.
    store.set_palette(viso::ui::Palette::dark());
    let root = build_scene(&mut store);
    store.layout(root, surface_rect(), &mut Vec::new());
    let first = store.arena().first_child(root).unwrap();
    assert_eq!(store.bounds(first).w, 200.0);
    assert_eq!(store.bounds(first).h, 24.0, "a bar is its own height");
    assert_eq!(store.slider(first).unwrap().marks, 10);

    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [W as f32, H as f32]);
    let actual = gpu.read_pixels_bgra8(surface);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/slider_widget.bgra8");
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read(&path)
        .unwrap_or_else(|_| panic!("missing golden {}; run with BLESS=1", path.display()));
    assert_eq!(actual.len(), expected.len());
    let worst = actual
        .iter()
        .zip(&expected)
        .map(|(a, e)| a.abs_diff(*e))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= TOL,
        "golden mismatch: max per-channel diff {worst}"
    );
}

// --- interactive input tapes ------------------------------------------------

/// A component built alone over the surface with the stores its handlers
/// write into, driven through the routers as the facade drives it.
struct Driven {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    projectors: SemanticProjector,
    /// The view the component sits in, at the surface's top start.
    top: NodeId,
    /// The component's node.
    root: NodeId,
    chain: Vec<NodeId>,
}

impl Driven {
    fn new(widget: impl Component + 'static) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let top = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            view(ViewStyle {
                size: Sizing::fixed(220.0, 24.0),
                ..Default::default()
            })
            .children(move |cx| widget.build(cx))
            .build(&mut cx);
            cx.root().expect("a root")
        };
        store.layout(top, Rect::new(0.0, 0.0, 220.0, 24.0), &mut Vec::new());
        let root = store.arena().first_child(top).expect("the component");
        Driven {
            store,
            states,
            bindings,
            projectors,
            top,
            root,
            chain: Vec::new(),
        }
    }

    /// Apply the writes a dispatch made, as the frame does.
    fn flush(&mut self) {
        KeyRouter::route_focus(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut TextEdits::new(),
        );
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        self.projectors
            .wake(&changed, &self.states, &mut self.store);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut EffectStore::new(),
            &mut VirtualLists::new(),
            &changed,
        );
    }

    fn pointer(&mut self, x: f32, y: f32, phase: PointerPhase, shift: bool) {
        let ev = PointerEvent {
            x,
            y,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers {
                shift,
                ..Modifiers::default()
            },
        };
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.top,
            ev,
            &mut self.chain,
        );
        self.flush();
    }

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
            &mut TextEdits::new(),
            self.top,
            ev,
            &mut self.chain,
        );
        self.flush();
    }

    /// The slider node: the root.
    fn slider(&self) -> viso::ui::slider::Slider {
        *self.store.slider(self.root).expect("a slider")
    }
}

/// Whether `heard` is `want`, each within a rounding of it.
fn near(heard: &[f32], want: &[f32]) -> bool {
    heard.len() == want.len() && heard.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-3)
}

/// A `[0, 100]` slider, 220 wide: its thumb's centre travels 200 from
/// x = 10.
fn volume(heard: &Rc<RefCell<Vec<f32>>>) -> Driven {
    let h = Rc::clone(heard);
    Driven::new(
        slider("Volume")
            .range(0.0, 100.0)
            .value(50.0)
            .size(Sizing::fill())
            .on_change(move |_, v| h.borrow_mut().push(v)),
    )
}

#[test]
fn a_press_jumps_the_thumb_a_drag_carries_it_and_shift_moves_it_finely() {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let mut s = volume(&heard);
    assert!(s.store.focusable(s.root));
    // On the thumb (centre at x = 110), it is picked up where it is.
    s.pointer(112.0, 12.0, PointerPhase::Down, false);
    assert!(heard.borrow().is_empty());
    assert_eq!(s.store.capture(), Some(s.root));
    assert_eq!(s.slider().held(), Some(Thumb::High));
    s.pointer(132.0, 12.0, PointerPhase::Move, false);
    assert!(
        near(&heard.borrow(), &[60.0]),
        "20 along of 200 is a tenth more"
    );
    assert_eq!(
        s.slider().shown[1],
        0.6,
        "the thumb follows the drag at once"
    );
    s.pointer(152.0, 12.0, PointerPhase::Move, true);
    assert!(
        near(&heard.borrow()[1..], &[61.0]),
        "Shift moves a tenth as far"
    );
    s.pointer(9000.0, 12.0, PointerPhase::Move, false);
    assert_eq!(heard.borrow().last(), Some(&100.0), "captured, it clamps");
    s.pointer(9000.0, 12.0, PointerPhase::Up, false);
    assert_eq!(s.store.capture(), None);
    assert_eq!(s.slider().held(), None);
    // On the track, the thumb jumps there.
    heard.borrow_mut().clear();
    s.pointer(60.0, 12.0, PointerPhase::Down, false);
    assert!(near(&heard.borrow(), &[25.0]));
    s.pointer(60.0, 12.0, PointerPhase::Up, false);
}

#[test]
fn the_keys_step_and_the_thumb_eases_there() {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let mut s = volume(&heard);
    focus_node(&mut s.store, s.root);
    s.key(Key::Right);
    s.key(Key::PageUp);
    s.key(Key::Left);
    assert!(
        near(&heard.borrow(), &[51.0, 61.0, 60.0]),
        "a hundredth, then ten"
    );
    let to = s.slider().value[1];
    assert!((to - 0.6).abs() < 1e-6);
    assert!(s.slider().shown[1] < to, "it eases from where it stood");
    for _ in 0..60 {
        s.store.tick_sliders(Duration::from_millis(16));
    }
    assert_eq!(s.slider().shown[1], to);
    s.key(Key::End);
    s.key(Key::Home);
    assert_eq!(&heard.borrow()[3..], [100.0, 0.0]);
    let state = s.store.semantic_state(s.root).unwrap();
    assert_eq!((state.value, state.range), (Some(0.0), Some((0.0, 100.0))));
    assert_eq!(s.store.semantics(s.root).unwrap().role, Role::Slider);
}

#[test]
fn a_range_keeps_its_thumbs_apart_each_a_slider_of_its_own() {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let h = Rc::clone(&heard);
    let mut s = Driven::new(
        range_slider("Price")
            .range(0.0, 100.0)
            .values(20.0, 70.0)
            .size(Sizing::fill())
            .on_change(move |_, lo, hi| h.borrow_mut().push((lo, hi))),
    );
    assert_eq!(s.store.semantics(s.root).unwrap().role, Role::Group);
    let thumbs: Vec<NodeId> = s.store.arena().children(s.root).collect();
    assert_eq!(thumbs.len(), 2);
    for (&t, name) in thumbs.iter().zip(["Minimum", "Maximum"]) {
        let sem = s.store.semantics(t).unwrap();
        assert_eq!((sem.role, sem.label.as_deref()), (Role::Slider, Some(name)));
        assert!(s.store.focusable(t));
    }
    // A press near the lower thumb (x = 50) carries it, and it stops at the
    // upper.
    s.pointer(20.0, 12.0, PointerPhase::Down, false);
    assert_eq!(*heard.borrow(), [(5.0, 70.0)]);
    assert_eq!(
        s.store.focused(),
        Some(thumbs[0]),
        "its thumb takes the focus"
    );
    s.pointer(500.0, 12.0, PointerPhase::Move, false);
    assert_eq!(heard.borrow().last(), Some(&(70.0, 70.0)));
    s.pointer(500.0, 12.0, PointerPhase::Up, false);
    // The keys reach the focused thumb, which stays below the upper.
    s.key(Key::Right);
    assert_eq!(heard.borrow().len(), 2, "the lower cannot pass the upper");
    s.key(Key::Left);
    assert_eq!(heard.borrow().last(), Some(&(69.0, 70.0)));
    assert_eq!(
        s.slider().focus,
        Some(Thumb::Low),
        "the focused thumb is ringed"
    );
    let v = s.slider().value;
    assert!((v[0] - 0.69).abs() < 1e-6 && v[1] == 0.7, "{v:?}");
}

#[test]
fn a_knob_turns_with_a_drag_up_and_names_its_value() {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let h = Rc::clone(&heard);
    let mut s = Driven::new(
        knob("Gain")
            .range(0.0, 10.0)
            .on_change(move |_, v| h.borrow_mut().push(v)),
    );
    let r = s.store.bounds(s.root);
    assert_eq!((r.w, r.h), (40.0, 40.0), "a knob is its own size");
    s.pointer(20.0, 20.0, PointerPhase::Down, false);
    assert!(heard.borrow().is_empty(), "a press does not jump a knob");
    s.pointer(20.0, -30.0, PointerPhase::Move, false);
    assert!(
        near(&heard.borrow(), &[2.5]),
        "50 up of 200 is a quarter turn"
    );
    s.pointer(70.0, -30.0, PointerPhase::Move, false);
    assert!(near(&heard.borrow()[1..], &[5.0]), "and right turns it on");
    s.pointer(70.0, -30.0, PointerPhase::Up, false);
    assert_eq!(s.store.semantics(s.root).unwrap().role, Role::Slider);
}

// --- allocation profile ----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `toggle_widget.rs`.
struct CountingAlloc;
// Thread-local counters: cargo runs the `#[test]`s in this binary in parallel
// on separate threads, all sharing this one process-global allocator. A
// process-global armed flag / counter would let a sibling test's allocations,
// happening on another thread while this test is inside its armed measurement
// window, race into this test's count and make the steady-state assertion
// flaky. Scoping arm state and the count to the measuring thread makes each
// test see only its own allocations — the frame path under test is
// synchronous, so every allocation it performs is on the arming thread. The
// `.load`/`.store`/`.fetch_add` API and its `Ordering` argument are kept so the
// call sites and the `GlobalAlloc` impl below are unchanged (the ordering is
// irrelevant for thread-local state and is ignored).
thread_local! {
    static ALLOCS_CELL: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ARMED_CELL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
struct TlsBool;
struct TlsUsize;
impl TlsBool {
    fn load(&self, _: Ordering) -> bool {
        ARMED_CELL.with(std::cell::Cell::get)
    }
    fn store(&self, v: bool, _: Ordering) {
        ARMED_CELL.with(|c| c.set(v));
    }
}
impl TlsUsize {
    fn load(&self, _: Ordering) -> usize {
        ALLOCS_CELL.with(std::cell::Cell::get)
    }
    fn store(&self, v: usize, _: Ordering) {
        ALLOCS_CELL.with(|c| c.set(v));
    }
    fn fetch_add(&self, v: usize, _: Ordering) {
        ALLOCS_CELL.with(|c| c.set(c.get() + v));
    }
}
static ALLOCS: TlsUsize = TlsUsize;
static ARMED: TlsBool = TlsBool;

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is a relaxed counter increment on allocation while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `layout` is the caller's, valid for `alloc`; the system allocator gets it unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by `System` with `layout`, through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `ptr` was allocated by `System` with `layout`, and the caller upholds `realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

struct Harness {
    gpu: CpuRaster,
    renderer: Renderer,
    surface: viso::gpu::SurfaceId,
    store: NodeStore,
    root: NodeId,
    primitives: Vec<viso::render::Primitive>,
}

fn setup_alloc() -> Harness {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let root = build_scene(&mut store);

    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    Harness {
        gpu,
        renderer,
        surface,
        store,
        root,
        primitives: Vec::new(),
    }
}

fn frame(h: &mut Harness) {
    h.primitives.clear();
    paint_tree(&h.store, h.root, &mut h.primitives);
    h.renderer.upload(&mut h.gpu, &h.primitives);
    h.renderer
        .submit(&mut h.gpu, h.surface, CLEAR, [W as f32, H as f32]);
}

#[test]
fn steady_slider_frame_is_allocation_free() {
    let mut h = setup_alloc();

    // Warm up until the frame path reaches steady state: the first frames grow the
    // persistent instance/mesh buffers to fit the scene, cache the per-pipeline
    // bind groups, and size the headless framebuffer/target pool.
    for _ in 0..4 {
        frame(&mut h);
    }
    // Grow the reused paint buffer to its steady capacity so a later `paint_tree`
    // into it does not reallocate.
    h.primitives.clear();
    paint_tree(&h.store, h.root, &mut h.primitives);

    let buffers = h.gpu.buffer_count();
    let textures = h.gpu.texture_count();
    let bind_groups = h.gpu.bind_group_count();
    let stats = h.renderer.frame_stats();

    let mut frame_allocs = [0usize; 2];
    for (i, slot) in frame_allocs.iter_mut().enumerate() {
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        frame(&mut h);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);

        assert_eq!(
            h.renderer.frame_stats(),
            stats,
            "frame {i}: frame_stats changed for an unchanged Slider scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Slider scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Slider scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Slider scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a Slider frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
        ..
    } = stats;
    assert!(draw_calls > 0, "the Slider scene must emit draw calls");
    assert!(instances > 0, "the Slider scene must emit instances");
}
