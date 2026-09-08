//! Section 71 validation pack for the `NavigationStack` control — a stack of
//! pages of which only the top is visible — driven through the public facade.
//! Mirrors `tabs_widget.rs` (golden + input tapes + a11y + allocation) with the
//! navigation stack's own structure: every page builds once into an overlapping
//! stretched column, and all but the top fold out of layout and paint through the
//! retained `hidden` flag. A navigation (a keyboard back gesture or a programmatic
//! `NavHandle::push`/`pop`) writes a reactive `depth` cell, defers a pair of
//! `hidden` flips the router applies (hide the old top, show the new), and fires
//! `on_navigate` once with the new depth.
//!
//! - **golden screenshot** — build a `NavigationStack` with two colored pages, the
//!   root on top, lay it out over a fixed surface, and confirm the pixels match a
//!   blessed baseline. Only the top page is shown, so the golden proves a hidden
//!   page paints nothing (its region shows the top page / clear). Pure quads — no
//!   font fixture (a navigation stack has no internal captions; the pages are solid
//!   fills);
//! - **keyboard input tape** — with the stack focused, `KeyRouter::route_key` pops
//!   on Escape and Backspace, flipping the pages' `hidden` flags and firing
//!   `on_navigate`; an unfocused stack receives no key dispatch, and a back gesture
//!   at the root is a clamped no-op;
//! - **programmatic input tape** — a captured `NavHandle`, driven inside an
//!   `EventCx` as the router would run it, pushes and pops: the flips it defers are
//!   applied, the `depth` cell moves, and `on_navigate` fires once; a push past the
//!   top and a pop below the root are clamped no-ops;
//! - **a11y snapshot** — the derived tree carries a `Navigation` root over `Group`
//!   pages; the root is focusable and declares a key handler for the back gesture;
//! - **allocation profile** — a warmed-up `NavigationStack` frame allocates nothing
//!   per frame (architecture section 47 hot-path contract), plus a *push-then-pop
//!   flip* frame: driving a push through the deferred seam flips `hidden` (which
//!   marks `LAYOUT | PAINT`), so the flip re-runs `store.layout`; the flip's
//!   one-time layout cost folds into warmup and the steady assert holds only after
//!   the scene settles — showing and hiding pages does not leak an allocation per
//!   frame.
//!
//! Building through `NavigationStack::build` is the point: it proves the widget
//! lowers to a page-switching subtree the `viso-ui` input/paint path and
//! `paint_tree` handle unchanged — including the deferred `hidden` seam a
//! push/pop needs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    BindingTable, BoxStyle, BuildCx, Component, EventCx, Key, KeyEvent, KeyRouter, LeafStyle,
    Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, Role,
    SemanticProjector, Size, StateId, StateStore, StateValue, TextEdits, VirtualLists, paint_tree,
};
use viso::widgets::{NavHandle, NavHandleSlot, navigation_stack};

const W: u32 = 240;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// The root page's fill — a distinct color so the golden shows which page is
/// currently on top.
const PAGE_A: Rgba = Rgba {
    r: 0.16,
    g: 0.22,
    b: 0.34,
    a: 1.0,
};
/// The second page's fill — a second distinct color; hidden in the golden (the
/// root is on top), so it must paint nothing.
const PAGE_B: Rgba = Rgba {
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

/// A page content builder that fills the page with a solid color, so the golden
/// shows the top page's region and a hidden page contributes nothing.
fn page_fill(color: Rgba) -> impl Fn(&mut BuildCx<'_>) + 'static {
    move |cx| {
        cx.leaf(LeafStyle {
            size: Size::fill(),
            style: BoxStyle::solid(color),
        });
    }
}

/// Build a `NavigationStack` that fills the surface with two colored pages, the
/// root on top, and return the root. NavigationStack authors a reactive cell, so
/// it builds through a reactive cx; the golden only paints (the routers are
/// exercised separately), so throwaway state stores are fine here.
fn build_scene(store: &mut NodeStore) -> NodeId {
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let widget = navigation_stack()
        .page(page_fill(PAGE_A))
        .page(page_fill(PAGE_B))
        .size(Size::fill());

    let mut cx = BuildCx::with_reactive(
        store,
        &mut states,
        &mut bindings,
        &mut lists,
        &mut text_edits,
        &mut projectors,
    );
    widget.build(&mut cx);
    cx.root().expect("navigation stack declares a root")
}

/// Walk a node's direct children (arena sibling chain).
fn children_of(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
    let arena = store.arena();
    let mut out = Vec::new();
    let mut child = arena.links(parent).and_then(|l| l.first_child);
    while let Some(c) = child {
        out.push(c);
        child = arena.links(c).and_then(|l| l.next_sibling);
    }
    out
}

// --- golden screenshot ------------------------------------------------------

#[test]
fn navigation_stack_renders_only_the_top_page_and_matches_golden() {
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/navigation_stack_widget.bgra8")
}

// --- interactive input tapes ------------------------------------------------

/// A `NavigationStack` laid out as the root of its own tree, kept together with
/// the reactive stores its handlers write into so a router (and a captured
/// `NavHandle`) can drive it. The stack fills the surface. `on_navigate` records
/// the last depth and bumps a counter.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    chain: Vec<NodeId>,
    handle: NavHandle,
    /// The shared `depth` cell the build authored — NavigationStack authors exactly
    /// one `Int` state cell, so a fresh `StateStore` allocating one `Int` yields the
    /// same handle the build produced (there is no public bare-`StateId` ctor).
    depth: StateId,
}

impl Interactive {
    fn new(counter: Rc<Cell<u32>>, last: Rc<Cell<Option<usize>>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();

        let slot: NavHandleSlot = Rc::new(RefCell::new(None));
        let widget = navigation_stack()
            .page(page_fill(PAGE_A))
            .page(page_fill(PAGE_B))
            .page(page_fill(PAGE_A))
            .size(Size::fill())
            .handle(&slot)
            .on_navigate(move |_ev, depth| {
                last.set(Some(depth));
                counter.set(counter.get() + 1);
            });

        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            widget.build(&mut cx);
            cx.root().expect("navigation stack declares a root")
        };
        let handle = slot.borrow().clone().expect("build fills the handle slot");
        let depth = StateStore::new().alloc(StateValue::Int(0));

        let mut scratch = Vec::new();
        store.layout(root, surface_rect(), &mut scratch);

        Interactive {
            store,
            states,
            bindings,
            root,
            chain: Vec::new(),
            handle,
            depth,
        }
    }

    /// The pages of the stack (the root's direct children), in stack order.
    fn pages(&self) -> Vec<NodeId> {
        children_of(&self.store, self.root)
    }

    /// Route a key sample through the public `KeyRouter` to the focused node,
    /// applying any deferred `hidden` flips exactly as the facade's `on_input` does.
    fn key(&mut self, ev: KeyEvent) -> bool {
        KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut TextEdits::new(),
            self.root,
            ev,
            &mut self.chain,
        )
    }

    /// Drive a `NavHandle` action (push/pop) as a router would: run it inside a
    /// throwaway `EventCx`, take the deferred `hidden` flips, and apply them.
    fn drive(&mut self, act: impl FnOnce(&NavHandle, &mut EventCx<'_>)) {
        let ev = read_pointer();
        let handle = self.handle.clone();
        let hidden = {
            let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            act(&handle, &mut cx);
            cx.__take_hidden_requests()
        };
        for (id, h) in hidden {
            self.store.set_hidden(id, h);
        }
    }

    /// The current value of the shared depth cell (via a throwaway read cx).
    fn depth(&mut self) -> Option<i32> {
        let ev = read_pointer();
        let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
        match cx.get(self.depth) {
            Some(StateValue::Int(i)) => Some(i),
            _ => None,
        }
    }
}

/// A neutral pointer sample for read-only / handle-driven cx construction.
fn read_pointer() -> PointerEvent {
    PointerEvent {
        x: 0.0,
        y: 0.0,
        phase: PointerPhase::Move,
        buttons: PointerButtons::NONE,
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
fn keyboard_back_gesture_pops_switches_pages_and_fires_navigate() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<usize>));
    let mut ix = Interactive::new(count.clone(), last.clone());

    let pages = ix.pages();
    assert_eq!(pages.len(), 3, "one node per page");
    assert!(!ix.store.hidden(pages[0]), "the root page starts on top");
    assert!(ix.store.hidden(pages[1]), "deeper pages start hidden");

    // Unfocused: the key router has no target, so nothing fires.
    assert!(
        !ix.key(key_ev(Key::Escape, true, false)),
        "an unfocused stack receives no key dispatch"
    );
    assert_eq!(count.get(), 0);

    // Push twice programmatically to reach the top page, then focus the stack.
    ix.drive(|nav, ev| nav.push(ev));
    ix.drive(|nav, ev| nav.push(ev));
    assert_eq!(ix.depth(), Some(2), "two pushes reach the top page");
    assert!(!ix.store.hidden(pages[2]), "the top page is shown");
    let before = count.get();
    ix.store.set_focused(Some(ix.root));

    // Escape pops one page: reveals page 1, hides page 2, moves depth, fires once.
    assert!(ix.key(key_ev(Key::Escape, true, false)));
    assert_eq!(ix.depth(), Some(1), "Escape pops one page");
    assert_eq!(
        count.get(),
        before + 1,
        "the back gesture fires on_navigate"
    );
    assert_eq!(last.get(), Some(1), "on_navigate carries the new depth");
    assert!(ix.store.hidden(pages[2]), "the popped page is hidden");
    assert!(!ix.store.hidden(pages[1]), "the revealed page is shown");

    // Backspace pops to the root; a further back gesture at the root is a no-op.
    assert!(ix.key(key_ev(Key::Backspace, true, false)));
    assert_eq!(ix.depth(), Some(0), "Backspace pops to the root");
    let before = count.get();
    ix.key(key_ev(Key::Escape, true, false));
    assert_eq!(ix.depth(), Some(0), "a back gesture at the root is a no-op");
    assert_eq!(count.get(), before, "and does not fire");
}

#[test]
fn handle_push_and_pop_switch_pages_and_clamp() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<usize>));
    let mut ix = Interactive::new(count.clone(), last.clone());
    let pages = ix.pages();

    // Push from the root reveals page 1, fires once, flips hidden.
    ix.drive(|nav, ev| nav.push(ev));
    assert_eq!(ix.depth(), Some(1), "push moves the depth cell to 1");
    assert_eq!(count.get(), 1, "push fires on_navigate once");
    assert_eq!(last.get(), Some(1), "on_navigate carries the new depth");
    assert!(ix.store.hidden(pages[0]), "the old top is now hidden");
    assert!(!ix.store.hidden(pages[1]), "the new top is now shown");

    // Push to the top, then a push past the last page is a clamped no-op.
    ix.drive(|nav, ev| nav.push(ev));
    assert_eq!(ix.depth(), Some(2));
    let before = count.get();
    ix.drive(|nav, ev| nav.push(ev));
    assert_eq!(ix.depth(), Some(2), "push past the top is a no-op");
    assert_eq!(count.get(), before, "and does not fire again");

    // Pop returns toward the root; a pop below the root is a clamped no-op.
    ix.drive(|nav, ev| nav.pop(ev));
    assert_eq!(ix.depth(), Some(1), "pop returns to page 1");
    ix.drive(|nav, ev| nav.pop(ev));
    assert_eq!(ix.depth(), Some(0), "pop returns to the root");
    let before = count.get();
    ix.drive(|nav, ev| nav.pop(ev));
    assert_eq!(ix.depth(), Some(0), "pop below the root is a no-op");
    assert_eq!(count.get(), before, "and does not fire again");
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn navigation_stack_derives_a_navigation_over_group_pages() {
    let mut store = NodeStore::new();
    let root = build_scene(&mut store);
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Navigation, "the control is a Navigation");

    // Each page is a Group; the derived children of the Navigation root are pages.
    let page_roles: Vec<Role> = node.children.iter().map(|&i| tree.nodes[i].role).collect();
    assert_eq!(
        page_roles,
        vec![Role::Group, Role::Group],
        "each page is a Group under the Navigation root"
    );

    // The stack root is interactive: focusable with a key handler for the back
    // gesture.
    assert!(store.focusable(root), "the stack root is focusable");
    assert!(
        store.has_key_handler(root),
        "the stack root attaches a key handler for the back gesture"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `tabs_widget.rs`.
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
fn steady_navigation_stack_frame_is_allocation_free_including_after_a_page_flip() {
    let mut h = setup_alloc();

    // Flip the top page once through a direct `set_hidden` pair (the same effect
    // the router applies for a push): hide page 0, show page 1. The flip marks
    // LAYOUT | PAINT, so re-run layout to settle the new visibility before
    // measuring — the one-time flip/layout cost folds into warmup.
    let pages = children_of(&h.store, h.root);
    h.store.set_hidden(pages[0], true);
    h.store.set_hidden(pages[1], false);
    let mut scratch = Vec::new();
    h.store.layout(h.root, surface_rect(), &mut scratch);

    // Warm up until the frame path reaches steady state on the post-flip scene.
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
            "frame {i}: frame_stats changed for an unchanged NavigationStack scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged NavigationStack scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged NavigationStack scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged NavigationStack scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a NavigationStack frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(
        draw_calls > 0,
        "the NavigationStack scene must emit draw calls"
    );
    assert!(
        instances > 0,
        "the NavigationStack scene must emit instances"
    );
}
