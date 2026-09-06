//! Section 71 validation pack for the `Splitter` control — a draggable divider
//! between two resizable panes — driven through the public facade. Mirrors
//! `slider_widget.rs` (golden + a11y + allocation + the two interactive input
//! tapes) with the splitter's own structure: a focusable flex container holding
//! pane A (its build-time share of the main axis), a fixed divider bar leaf, and
//! pane B (filling the rest). A primary *drag* and an *arrow-key* step drive the
//! same `on_change` and move the reactive split-fraction cell.
//!
//! - **golden screenshot** — build a `Splitter` with `Splitter::build` (two
//!   colored pane leaves around the neutral bar), lay it out over a fixed surface,
//!   and confirm the pixels match a blessed baseline. Unlike the text controls the
//!   splitter has no glyph content, so the golden is pure quads — no font fixture;
//! - **pointer drag input tape** — `PointerRouter::route` over the splitter's world
//!   box: a primary press *captures the pointer to the splitter* (so subsequent
//!   samples route to it even outside its box), a move drags the fraction by the
//!   pixel delta over the extent and fires `on_change`, a move past the extent
//!   clamps to `1.0`, and the release frees the capture;
//! - **keyboard step input tape** — with the splitter focused, `KeyRouter::route_key`
//!   steps the fraction on the arrow keys (Right/Down up, Left/Up down) and fires
//!   `on_change`; an unfocused splitter receives no key dispatch;
//! - **a11y snapshot** — a `Splitter`'s derived semantics node is `Role::Group`
//!   with its authored label, and — being interactive — it *declares* both a
//!   pointer and a key handler and is focusable (the inverse of the presentational
//!   controls, whose derived node has no handlers);
//! - **allocation profile** — a warmed-up `Splitter` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `slider_widget.rs`.
//!
//! Building through `Splitter::build` is the point: it proves the widget lowers to
//! an interactive, content-bearing subtree the `viso-ui` input/paint path and
//! `paint_tree` handle unchanged — including pointer *capture*, which a drag needs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    BindingTable, BoxStyle, BuildCx, Component, Key, KeyEvent, KeyRouter, LeafStyle, Modifiers,
    NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Role, Size,
    StateStore, TextEdits, VirtualLists, paint_tree,
};
use viso::widgets::splitter;

const W: u32 = 240;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// Pane A's fill — a distinct color so the golden shows the split boundary.
const PANE_A: Rgba = Rgba {
    r: 0.16,
    g: 0.22,
    b: 0.34,
    a: 1.0,
};
/// Pane B's fill — a second distinct color.
const PANE_B: Rgba = Rgba {
    r: 0.30,
    g: 0.18,
    b: 0.20,
    a: 1.0,
};

fn surface_rect() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    }
}

/// A pane content builder that fills the pane with a solid color, so the golden
/// shows two distinct regions separated by the neutral divider bar.
fn pane_fill(color: Rgba) -> impl Fn(&mut BuildCx<'_>) + 'static {
    move |cx| {
        cx.leaf(LeafStyle {
            size: Size::fill(),
            style: BoxStyle::solid(color),
        });
    }
}

/// Build a `Splitter` that fills the surface, split 40/60 on the row axis with
/// two colored panes, and return the root. The `extent` is the surface width so
/// the drag arithmetic (`dx / extent`) checks cleanly.
fn build_scene(store: &mut NodeStore) -> NodeId {
    // The splitter authors reactive cells, so it must build through a reactive cx;
    // the golden only paints (the routers are exercised separately), so throwaway
    // state stores are fine here.
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();

    let widget = splitter("Editor / Preview")
        .fraction(0.4)
        .extent(W as f32)
        .size(Size::fill())
        .panes(pane_fill(PANE_A), pane_fill(PANE_B));

    let mut cx = BuildCx::with_reactive(
        store,
        &mut states,
        &mut bindings,
        &mut lists,
        &mut text_edits,
    );
    widget.build(&mut cx);
    cx.root().expect("splitter declares a root")
}

// --- golden screenshot ------------------------------------------------------

#[test]
fn splitter_renders_two_panes_and_a_divider_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let root = build_scene(&mut store);

    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [W as f32, H as f32]);
    let actual = gpu.read_pixels_bgra8(surface);

    let path = golden_path();
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        eprintln!("blessed golden: {}", path.display());
        return;
    }

    let expected = std::fs::read(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with BLESS=1 to generate it",
            path.display()
        )
    });
    assert_eq!(
        actual.len(),
        expected.len(),
        "golden size mismatch: {} vs {}",
        actual.len(),
        expected.len()
    );

    let mut worst = 0u8;
    let mut worst_at = 0usize;
    for (i, (&a, &e)) in actual.iter().zip(&expected).enumerate() {
        let diff = a.abs_diff(e);
        if diff > worst {
            worst = diff;
            worst_at = i;
        }
    }
    assert!(
        worst <= TOL,
        "golden mismatch: max per-channel diff {worst} at byte {worst_at} \
         (pixel {}, channel {}) exceeds tolerance {TOL}",
        worst_at / 4,
        worst_at % 4,
    );
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/splitter_widget.bgra8")
}

// --- interactive input tapes ------------------------------------------------

/// A focusable splitter laid out as the root of its own tree, kept together with
/// the reactive stores its handlers write into so a router can drive it. The
/// splitter fills the surface (root `Fill`) so any central pointer sample hits it.
/// Its `on_change` records the last carried fraction and bumps a counter.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    splitter: NodeId,
    chain: Vec<NodeId>,
}

impl Interactive {
    /// Build a fill splitter whose `on_change` records the new fraction and bumps
    /// `counter`, laid out over the surface so a center-of-surface pointer sample
    /// lands on it. `extent == W` and an initial fraction of `0.0` keep the drag
    /// arithmetic (`dx / extent`) easy to check.
    fn new(counter: Rc<Cell<u32>>, last: Rc<Cell<Option<f32>>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();

        let widget = splitter("Editor / Preview")
            .fraction(0.0)
            .extent(W as f32)
            .size(Size::fill())
            .panes(pane_fill(PANE_A), pane_fill(PANE_B))
            .on_change(move |_ev, f| {
                last.set(Some(f));
                counter.set(counter.get() + 1);
            });

        let splitter = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
            );
            widget.build(&mut cx);
            cx.root().expect("splitter declares a root")
        };

        let mut scratch = Vec::new();
        store.layout(splitter, surface_rect(), &mut scratch);

        Interactive {
            store,
            states,
            bindings,
            splitter,
            chain: Vec::new(),
        }
    }

    /// Route a pointer sample through the public `PointerRouter`, exactly as the
    /// facade's `on_input` does — including applying any capture the handler
    /// requests. Returns whether any handler ran.
    fn pointer(&mut self, ev: PointerEvent) -> bool {
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.splitter,
            ev,
            &mut self.chain,
        )
    }

    /// Route a key sample through the public `KeyRouter` to the focused node.
    fn key(&mut self, ev: KeyEvent) -> bool {
        KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut TextEdits::new(),
            self.splitter,
            ev,
            &mut self.chain,
        )
    }
}

/// A primary-button pointer sample at `(x, center)`, in the given phase.
fn primary_at(x: f32, phase: PointerPhase) -> PointerEvent {
    PointerEvent {
        x,
        y: H as f32 / 2.0,
        phase,
        buttons: PointerButtons::PRIMARY,
        modifiers: Modifiers::default(),
    }
}

fn key_ev(key: Key, pressed: bool, repeat: bool) -> KeyEvent {
    KeyEvent {
        key,
        pressed,
        repeat,
        modifiers: Modifiers::default(),
    }
}

#[test]
fn pointer_drag_moves_fraction_captures_and_fires_change() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<f32>));
    let mut ix = Interactive::new(count.clone(), last.clone());

    // A primary press near the left of the surface anchors the drag and captures
    // the pointer to the splitter — the router applies the request, so subsequent
    // samples route to the splitter even outside its box.
    assert!(
        ix.pointer(primary_at(20.0, PointerPhase::Down)),
        "the press hits the splitter's handler"
    );
    assert_eq!(
        ix.store.capture(),
        Some(ix.splitter),
        "the press captures the pointer to the splitter so a drag keeps tracking"
    );
    assert_eq!(count.get(), 0, "the press alone does not fire a change");

    // Move right by 60px over the W-px extent: +0.25 fraction. The splitter
    // started at 0.0, so the fraction lands at 0.25.
    assert!(ix.pointer(primary_at(80.0, PointerPhase::Move)));
    assert_eq!(count.get(), 1, "the move fires one change");
    assert_eq!(
        last.get(),
        Some(0.25),
        "on_change carries the fraction dragged by dx/extent"
    );

    // A sample far to the right of the surface still routes here because the
    // pointer is captured; it clamps the fraction to the far end (1.0).
    assert!(ix.pointer(primary_at(10000.0, PointerPhase::Move)));
    assert_eq!(
        last.get(),
        Some(1.0),
        "a captured drag past the extent clamps to 1.0"
    );

    // The release frees the capture.
    ix.pointer(primary_at(10000.0, PointerPhase::Up));
    assert_eq!(
        ix.store.capture(),
        None,
        "the release frees the pointer capture"
    );
}

#[test]
fn keyboard_step_requires_focus_and_moves_the_fraction() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<f32>));
    let mut ix = Interactive::new(count.clone(), last.clone());

    // Unfocused: the key router has no target, so nothing fires.
    assert!(
        !ix.key(key_ev(Key::Right, true, false)),
        "an unfocused splitter receives no key dispatch"
    );
    assert_eq!(count.get(), 0);

    ix.store.set_focused(Some(ix.splitter));

    // The splitter starts at 0.0. An arrow press steps 2% of the extent per press.
    assert!(ix.key(key_ev(Key::Right, true, false)));
    assert_eq!(last.get(), Some(0.02), "Right steps the fraction up 2%");
    assert!(ix.key(key_ev(Key::Down, true, false)));
    assert_eq!(last.get(), Some(0.04), "Down steps up too");
    assert!(ix.key(key_ev(Key::Left, true, false)));
    assert_eq!(last.get(), Some(0.02), "Left steps back down");
    assert!(ix.key(key_ev(Key::Up, true, false)));
    assert_eq!(last.get(), Some(0.0), "Up steps down too");

    ix.key(key_ev(Key::Right, false, false)); // key-up: ignored
    assert_eq!(
        count.get(),
        4,
        "each arrow press fired one change; the key-up did not"
    );
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn splitter_derives_a_group_semantics_node_named_by_its_label_and_is_interactive() {
    let mut store = NodeStore::new();
    let root = build_scene(&mut store);
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(
        node.role,
        Role::Group,
        "a Splitter is a resizable-pane group"
    );
    assert_eq!(
        node.label.as_deref(),
        Some("Editor / Preview"),
        "the accessible name is the authored label"
    );

    // Being interactive, the splitter's root declares both a pointer and a key
    // handler and is focusable — the inverse of the presentational controls.
    assert!(
        store.has_handler(root),
        "the splitter declares a pointer handler for dragging"
    );
    assert!(
        store.has_key_handler(root),
        "the splitter declares a key handler for arrow-key stepping"
    );
    assert!(
        store.focusable(root),
        "the splitter is focusable so the keyboard can drive it"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `slider_widget.rs`.
struct CountingAlloc;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is a relaxed counter increment on allocation while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

struct Harness {
    gpu: HeadlessRaster,
    renderer: Renderer,
    surface: viso::gpu::SurfaceId,
    store: NodeStore,
    root: NodeId,
    primitives: Vec<viso::render::Primitive>,
}

fn setup_alloc() -> Harness {
    let mut gpu = HeadlessRaster::new();
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
fn steady_splitter_frame_is_allocation_free() {
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
            "frame {i}: frame_stats changed for an unchanged Splitter scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Splitter scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Splitter scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Splitter scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a Splitter frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the Splitter scene must emit draw calls");
    assert!(instances > 0, "the Splitter scene must emit instances");
}
