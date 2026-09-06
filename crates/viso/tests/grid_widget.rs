//! Section 71 validation pack for the `Grid` widget — the third Tier 3 (layout
//! structure) control — driven through the public facade. `Grid` is a
//! two-dimensional track layout: children fall into the cells of a column×row
//! template, auto-flowing row-major or pinned to an explicit cell with a span.
//! It is non-interactive (a structural container, unlike the interactive Tier 2
//! controls) and non-reactive (a plain `BuildCx`, unlike `VirtualList`), so this
//! pack mirrors `scroll_widget.rs` with the track solver's geometry as its tape
//! in place of a pointer/scroll gesture:
//!
//! - **golden screenshot** — build a `Grid` with a 2×2 fractional template and
//!   four distinctly-colored cells with `Grid::build`, lay it out, paint it
//!   through the headless backend, and confirm the pixels match a blessed
//!   baseline. The gaps and per-cell colors prove a widget-built grid solves its
//!   tracks and paints each cell into its own box;
//! - **placement tape** — the geometry equivalent of an input tape: fold over a
//!   widget-built grid's laid-out cells and assert exactly where the track solver
//!   put them — auto-flow row-major across a two-column template, an explicit
//!   `place` pinning a cell to a column, and a `column_span` widening a cell over
//!   two tracks plus the gap between them;
//! - **a11y snapshot** — a `Grid`'s derived semantics node is `Role::Group` and
//!   carries an authored label; it declares no interactive handlers of its own;
//! - **allocation profile** — a warmed-up `Grid` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `scroll_widget.rs`.
//!
//! Building through `Grid::build` (not raw `cx.grid`) is the point: it proves the
//! widget lowers to the same retained grid node the primitive does, so the whole
//! `viso-ui` track-solving pass handles a widget-authored grid unchanged.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    BoxStyle, BuildCx, Component, GridPlacement, LeafStyle, NodeId, NodeStore, Role, Size,
    TrackSizing, paint_tree,
};
use viso::widgets::{GridViewStyle, grid};

const W: u32 = 120;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

const DARK: Rgba = Rgba {
    r: 0.1,
    g: 0.1,
    b: 0.12,
    a: 1.0,
};
const CELLS: [Rgba; 4] = [
    Rgba {
        r: 0.6,
        g: 0.2,
        b: 0.2,
        a: 1.0,
    },
    Rgba {
        r: 0.2,
        g: 0.6,
        b: 0.2,
        a: 1.0,
    },
    Rgba {
        r: 0.2,
        g: 0.2,
        b: 0.6,
        a: 1.0,
    },
    Rgba {
        r: 0.6,
        g: 0.6,
        b: 0.2,
        a: 1.0,
    },
];

/// The golden scene: a dark 100×100 `Grid` with a 2×2 fractional template, an
/// 8px gap, and four fill cells in distinct colors. Authored entirely through the
/// `Grid` widget and `cx.leaf`, so the pixels prove a widget-built grid solves
/// its two-by-two tracks, leaves the gap dark, and paints each cell into its box.
fn build_scene(store: &mut NodeStore) -> NodeId {
    let g = grid(GridViewStyle {
        columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
        rows: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
        column_gap: 8.0,
        row_gap: 8.0,
        size: Size::fixed(100.0, 100.0),
        background: BoxStyle::solid(DARK),
        ..Default::default()
    })
    .children(|cx| {
        for color in CELLS {
            cx.leaf(LeafStyle {
                size: Size::fill(),
                style: BoxStyle::solid(color),
            });
        }
    });

    let mut cx = BuildCx::new(store);
    g.build(&mut cx);
    cx.root().expect("grid declares a root")
}

fn surface_rect() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    }
}

// --- golden screenshot -----------------------------------------------------

fn render_scene() -> Vec<u8> {
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
    gpu.read_pixels_bgra8(surface)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/grid_widget.bgra8")
}

#[test]
fn grid_scene_matches_golden() {
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

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn grid_derives_a_group_semantics_node_with_label() {
    let mut store = NodeStore::new();
    let root = {
        let g = grid(GridViewStyle::default()).label("Dashboard");
        let mut cx = BuildCx::new(&mut store);
        g.build(&mut cx);
        cx.root().expect("grid declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "a Grid is a Group");
    assert_eq!(node.label.as_deref(), Some("Dashboard"));
    assert!(
        !store.has_handler(root) && !store.has_key_handler(root),
        "a Grid declares no interactive handlers of its own"
    );
}

// --- placement tape ---------------------------------------------------------

/// The direct children of `parent`, in sibling order — the cells the track solver
/// placed, read back for the geometry assertions below.
fn cells_of(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
    let arena = store.arena();
    let mut out = Vec::new();
    let mut child = arena.links(parent).and_then(|l| l.first_child);
    while let Some(c) = child {
        out.push(c);
        child = arena.links(c).and_then(|l| l.next_sibling);
    }
    out
}

/// A widget-built grid, laid out over a `w`×`h` surface, with `author` declaring
/// its cells — returns the store and root so a test can read cell bounds back.
fn placed_grid(
    style: GridViewStyle,
    w: f32,
    h: f32,
    author: impl Fn(&mut BuildCx<'_>) + 'static,
) -> (NodeStore, NodeId) {
    let mut store = NodeStore::new();
    let root = {
        let g = grid(style).children(author);
        let mut cx = BuildCx::new(&mut store);
        g.build(&mut cx);
        cx.root().expect("grid declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(
        root,
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        },
        &mut scratch,
    );
    (store, root)
}

#[test]
fn unplaced_cells_auto_flow_row_major_through_a_widget_grid() {
    // Two 50px columns, one 50px row; two unplaced fill cells auto-flow into the
    // two columns of the row.
    let (store, root) = placed_grid(
        GridViewStyle {
            columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
            rows: vec![TrackSizing::Fixed(50.0)],
            size: Size::fixed(100.0, 50.0),
            ..Default::default()
        },
        100.0,
        50.0,
        |cx| {
            cx.leaf(LeafStyle {
                size: Size::fill(),
                style: BoxStyle::NONE,
            });
            cx.leaf(LeafStyle {
                size: Size::fill(),
                style: BoxStyle::NONE,
            });
        },
    );

    let cells = cells_of(&store, root);
    assert_eq!(cells.len(), 2, "both authored cells attach under the grid");
    assert_eq!(
        store.bounds(cells[0]),
        Rect {
            x: 0.0,
            y: 0.0,
            w: 50.0,
            h: 50.0,
        },
        "the first cell auto-flows into column 0"
    );
    assert_eq!(
        store.bounds(cells[1]),
        Rect {
            x: 50.0,
            y: 0.0,
            w: 50.0,
            h: 50.0,
        },
        "the second cell auto-flows into column 1 of the same row"
    );
}

#[test]
fn place_pins_a_cell_to_an_explicit_column_through_a_widget_grid() {
    // A single cell pinned to column 1 lands at the second column's origin, not
    // the auto-flow column 0.
    let (store, root) = placed_grid(
        GridViewStyle {
            columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
            rows: vec![TrackSizing::Fixed(50.0)],
            size: Size::fixed(100.0, 50.0),
            ..Default::default()
        },
        100.0,
        50.0,
        |cx| {
            cx.place(GridPlacement {
                column: Some(1),
                row: Some(0),
                column_span: 1,
                row_span: 1,
            });
            cx.leaf(LeafStyle {
                size: Size::fill(),
                style: BoxStyle::NONE,
            });
        },
    );

    let cells = cells_of(&store, root);
    assert_eq!(
        store.bounds(cells[0]),
        Rect {
            x: 50.0,
            y: 0.0,
            w: 50.0,
            h: 50.0,
        },
        "the pinned cell sits in column 1 at that cell's full extent"
    );
}

#[test]
fn a_column_span_widens_a_cell_over_two_tracks_and_the_gap() {
    // Two 50px columns with a 10px gap; a cell spanning both columns covers
    // 50 + 10 + 50 = 110px from the first column's origin.
    let (store, root) = placed_grid(
        GridViewStyle {
            columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
            rows: vec![TrackSizing::Fixed(50.0)],
            column_gap: 10.0,
            size: Size::fixed(110.0, 50.0),
            ..Default::default()
        },
        110.0,
        50.0,
        |cx| {
            cx.place(GridPlacement {
                column: Some(0),
                row: Some(0),
                column_span: 2,
                row_span: 1,
            });
            cx.leaf(LeafStyle {
                size: Size::fill(),
                style: BoxStyle::NONE,
            });
        },
    );

    let cells = cells_of(&store, root);
    assert_eq!(
        store.bounds(cells[0]),
        Rect {
            x: 0.0,
            y: 0.0,
            w: 110.0,
            h: 50.0,
        },
        "a two-column span covers both tracks plus the gap between them"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `scroll_widget.rs`.
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
fn steady_grid_frame_is_allocation_free() {
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
            "frame {i}: frame_stats changed for an unchanged Grid scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Grid scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Grid scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Grid scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a Grid frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the Grid scene must emit draw calls");
    assert!(instances > 0, "the Grid scene must emit instances");
}
