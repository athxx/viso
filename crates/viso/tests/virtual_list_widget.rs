//! Section 71 validation pack for the `VirtualList` widget — the second Tier 3
//! (layout structure) control — driven through the public facade. `VirtualList`
//! is a virtualized scrolling list: a scroll viewport over a fixed-extent canvas
//! sized to the whole logical collection, so only a window of rows is ever
//! mounted while the scroll range spans every item. This pack mirrors
//! `scroll_widget.rs`, with the list's virtualization drive as its interactive
//! tape:
//!
//! - **golden screenshot** — build a `VirtualList` with `VirtualList::build`,
//!   run one frame of the driver's reconcile → relayout → absorb sequence to
//!   mount the first window of rows, paint it through the headless backend, and
//!   confirm the pixels match a blessed baseline. The collection is far taller
//!   than the viewport, so the golden proves the list clips to its box and paints
//!   only the mounted window;
//! - **virtualization tape** — the same frame drive `virtual_list_seam.rs` folds
//!   over the raw `cx.virtual_list` node, here over a widget-built tree: a 100k-row
//!   list mounts ~a window (never 100k), reports the full scroll range, a sub-row
//!   scroll rebinds nothing, and crossing three rows recycles exactly three;
//! - **a11y snapshot** — a `VirtualList`'s derived semantics node is `Role::Group`
//!   and carries an authored label; it declares no interactive handlers of its own
//!   (the scroll gesture is routed by `viso-ui`, not a widget handler);
//! - **allocation profile** — a warmed-up steady `VirtualList` frame (paint of a
//!   settled window) allocates nothing per frame (architecture section 47
//!   hot-path contract), same CountingAlloc + `frame_stats`/`*_count()`
//!   steady-state asserts as `scroll_widget.rs`.
//!
//! Building through `VirtualList::build` (not raw `cx.virtual_list`) is the point:
//! it proves the widget lowers to the same retained viewport+canvas the primitive
//! does, so the whole `viso-ui` virtualization pipeline handles a widget-authored
//! list unchanged.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, Component, DirtyClass, EffectStore, LeafStyle, Length,
    NodeId, NodeStore, Role, SemanticProjector, Size, StateStore, TextEdits, Vec2, VirtualLists,
    paint_tree, virtual_list as vl_driver,
};
use viso::widgets::{VirtualListViewStyle, virtual_list};

const W: u32 = 120;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

const VIEWPORT_W: f32 = 100.0;
const VIEWPORT_H: f32 = 100.0;
const ROW_H: f32 = 30.0;
const OVERSCAN: u32 = 4;

const DARK: Rgba = Rgba {
    r: 0.1,
    g: 0.1,
    b: 0.12,
    a: 1.0,
};
const TEAL: Rgba = Rgba {
    r: 0.1,
    g: 0.5,
    b: 0.5,
    a: 1.0,
};

/// The driver's frame-phase inputs for a virtual list, threaded together exactly
/// as `AppDriver` does each frame: the node store, the sibling reactive stores,
/// the list registry, the viewport id, and the reusable layout scratch.
struct Seam {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    viewport: NodeId,
    surface: Rect,
    scratch: Vec<u32>,
    redo: Vec<NodeId>,
}

impl Seam {
    /// Author a vertical `item_count`-row list through `VirtualList::build`, each
    /// row a single fixed-height teal leaf. This is how an app's `build` declares
    /// a list — the widget lowers to the same viewport+canvas the seam drives.
    fn build(item_count: usize, background: BoxStyle) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let viewport = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            virtual_list(VirtualListViewStyle {
                axis: Axis::Column,
                size: Size::fixed(VIEWPORT_W, VIEWPORT_H),
                overscan: OVERSCAN,
                estimated_row: ROW_H,
                background,
            })
            .items(item_count, |_index, cx| {
                cx.leaf(LeafStyle {
                    size: Size {
                        width: Length::fill(),
                        height: Length::Fixed(ROW_H),
                    },
                    style: BoxStyle::solid(TEAL),
                });
            })
            .build(&mut cx);
            cx.root().expect("virtual_list declares a root")
        };
        // The launch path seeds the whole tree dirty so the first layout resolves
        // every box; do the same here.
        store.mark_dirty(
            viewport,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
        Seam {
            store,
            states,
            bindings,
            effects: EffectStore::new(),
            lists,
            viewport,
            surface: Rect {
                x: 0.0,
                y: 0.0,
                w: VIEWPORT_W,
                h: VIEWPORT_H,
            },
            scratch: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// One frame's `Layout` phase, exactly as `AppDriver` runs it: reconcile the
    /// lists, incrementally relayout the invalidated subtrees, absorb the freshly
    /// measured row heights. Returns the number of rows (re)bound this frame.
    fn frame(&mut self) -> u32 {
        if self.store.bounds_main(self.viewport, Axis::Column) <= 0.0 {
            self.store
                .layout(self.viewport, self.surface, &mut self.scratch);
        }
        let bound = vl_driver::reconcile(
            &mut self.store,
            &mut self.lists,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
        );
        self.store.relayout_dirty(
            self.viewport,
            self.surface,
            &mut self.scratch,
            &mut self.redo,
        );
        vl_driver::absorb_measurements(&self.store, &mut self.lists);
        self.store.clear_dirty();
        bound
    }
}

// --- golden screenshot -----------------------------------------------------

fn surface_rect() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    }
}

/// The golden scene: a dark `VirtualList` viewport (100×100) over a 20-row
/// collection of 30px teal rows (600px of logical extent, overflowing the box).
/// One frame of the driver's reconcile mounts the first window; painting then
/// proves a widget-built list clips to its box and paints only mounted rows.
fn render_scene() -> Vec<u8> {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut seam = Seam::build(20, BoxStyle::solid(DARK));
    seam.frame();

    let mut primitives = Vec::new();
    paint_tree(&seam.store, seam.viewport, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [W as f32, H as f32]);
    gpu.read_pixels_bgra8(surface)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/virtual_list_widget.bgra8")
}

#[test]
fn virtual_list_scene_matches_golden() {
    let actual = render_scene();
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

// --- virtualization tape ----------------------------------------------------

const ITEM_COUNT: usize = 100_000;

/// A 100k-row widget-built list mounts only a window's worth of nodes yet reports
/// the full scroll range — the core virtualization contract over a facade tree.
#[test]
fn a_widget_built_list_mounts_a_window_not_the_whole_collection() {
    let mut seam = Seam::build(ITEM_COUNT, BoxStyle::NONE);
    seam.frame();

    let mounted = seam.lists.get(seam.viewport).unwrap().mounted_count();
    // Visible span ≈ 3 rows (100px / 30px), plus 2·overscan → ~11 mounted.
    assert!(
        (3..=20).contains(&mounted),
        "mounted {mounted} should be ~visible+2·overscan, not {ITEM_COUNT}"
    );
    assert_eq!(
        seam.store.scroll_range(seam.viewport, Axis::Column),
        ITEM_COUNT as f32 * ROW_H - VIEWPORT_H
    );
}

/// A scroll that stays within the mounted window is a pure transform: the frame's
/// reconcile rebinds nothing, so the list stays on the zero-relayout steady path.
#[test]
fn a_sub_window_scroll_stays_on_the_steady_path() {
    let mut seam = Seam::build(ITEM_COUNT, BoxStyle::NONE);
    seam.frame();
    let mounted_before = seam.lists.get(seam.viewport).unwrap().mounted_count();

    seam.store
        .scroll_by(seam.viewport, Vec2 { x: 0.0, y: 10.0 });
    let bound = seam.frame();
    assert_eq!(bound, 0, "a sub-row scroll rebinds nothing");
    assert_eq!(
        seam.lists.get(seam.viewport).unwrap().mounted_count(),
        mounted_before,
        "the mounted window is untouched"
    );
}

/// Crossing a row boundary recycles a bounded handful: advancing the window by
/// three rows binds exactly three, with the mounted count stable.
#[test]
fn crossing_a_boundary_recycles_a_bounded_handful() {
    let mut seam = Seam::build(ITEM_COUNT, BoxStyle::NONE);
    seam.frame();
    seam.store.scroll_by(
        seam.viewport,
        Vec2 {
            x: 0.0,
            y: 30_000.0,
        },
    );
    seam.frame();
    let mounted_before = seam.lists.get(seam.viewport).unwrap().mounted_count();

    seam.store
        .scroll_by(seam.viewport, Vec2 { x: 0.0, y: 90.0 });
    let bound = seam.frame();
    assert_eq!(bound, 3, "advancing by 3 rows binds exactly 3");
    assert_eq!(
        seam.lists.get(seam.viewport).unwrap().mounted_count(),
        mounted_before,
        "the mounted window size is stable across a boundary crossing"
    );
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn virtual_list_derives_a_group_semantics_node_with_label() {
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
        virtual_list(VirtualListViewStyle::default())
            .items(10, |_i, cx| {
                cx.leaf(LeafStyle::default());
            })
            .label("Messages")
            .build(&mut cx);
        cx.root().expect("virtual_list declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "a VirtualList is a Group");
    assert_eq!(node.label.as_deref(), Some("Messages"));
    assert!(
        !store.has_handler(root) && !store.has_key_handler(root),
        "a VirtualList declares no interactive handlers of its own"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `scroll_widget.rs`.
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
    seam: Seam,
    primitives: Vec<viso::render::Primitive>,
}

fn setup_alloc() -> Harness {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);

    // A settled window: build a 20-row list and mount its first window. Once
    // mounted, a paint of an unscrolled list is the steady state.
    let mut seam = Seam::build(20, BoxStyle::solid(DARK));
    seam.frame();

    Harness {
        gpu,
        renderer,
        surface,
        seam,
        primitives: Vec::new(),
    }
}

fn frame(h: &mut Harness) {
    h.primitives.clear();
    paint_tree(&h.seam.store, h.seam.viewport, &mut h.primitives);
    h.renderer.upload(&mut h.gpu, &h.primitives);
    h.renderer
        .submit(&mut h.gpu, h.surface, CLEAR, [W as f32, H as f32]);
}

#[test]
fn steady_virtual_list_frame_is_allocation_free() {
    let mut h = setup_alloc();

    // Warm up until the frame path reaches steady state: the first frames grow
    // the persistent instance buffers to fit the scene, cache the per-pipeline
    // bind groups, and size the headless framebuffer/target pool.
    for _ in 0..8 {
        frame(&mut h);
    }
    // Grow the reused paint buffer to its steady capacity so a later `paint_tree`
    // into it does not reallocate.
    h.primitives.clear();
    paint_tree(&h.seam.store, h.seam.viewport, &mut h.primitives);

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
            "frame {i}: frame_stats changed for an unchanged VirtualList scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged VirtualList scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged VirtualList scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged VirtualList scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a VirtualList frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the VirtualList scene must emit draw calls");
    assert!(instances > 0, "the VirtualList scene must emit instances");
}
