//! The `Tabs` control's pixels and frame cost: a strip of tab buttons above
//! the selected tab's panel matches a blessed golden, the hidden panel
//! painting nothing; a warmed-up frame allocates nothing, after a tab flip
//! too.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::{FrameStats, GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs};
use viso::ui::{
    BindingTable, BoxStyle, BuildCx, Component, Content, LeafStyle, NodeId, NodeStore,
    SemanticProjector, Sizing, StateStore, TextEdits, Vec2, VirtualLists, paint_tree,
};
use viso::widgets::tabs;

const W: u32 = 240;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// The first panel's fill — a distinct color so the golden shows which panel is
/// currently revealed.
const PANEL_A: Rgba = Rgba {
    r: 0.16,
    g: 0.22,
    b: 0.34,
    a: 1.0,
};
/// The second panel's fill — a second distinct color; hidden in the golden (tab 0
/// is selected), so it must paint nothing.
const PANEL_B: Rgba = Rgba {
    r: 0.30,
    g: 0.18,
    b: 0.20,
    a: 1.0,
};

const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
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

/// A panel content builder that fills the panel with a solid color, so the golden
/// shows the revealed panel's region and a hidden panel contributes nothing.
fn panel_fill(color: Rgba) -> impl Fn(&mut BuildCx<'_>) + 'static {
    move |cx| {
        cx.leaf(LeafStyle {
            size: Sizing::fill(),
            style: BoxStyle::solid(color),
        });
    }
}

/// Build a `Tabs` that fills the surface with two colored panels, tab 0 selected,
/// and return the root. Tabs authors a reactive cell, so it builds through a
/// reactive cx; the golden only paints (the routers are exercised separately), so
/// throwaway state stores are fine here.
fn build_scene(store: &mut NodeStore) -> NodeId {
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let widget = tabs()
        .tab("Details", panel_fill(PANEL_A))
        .tab("History", panel_fill(PANEL_B))
        .selected(0)
        .size(Sizing::fill());

    let mut cx = BuildCx::with_reactive(
        store,
        &mut states,
        &mut bindings,
        &mut lists,
        &mut text_edits,
        &mut projectors,
    );
    widget.build(&mut cx);
    cx.root().expect("tabs declares a root")
}

/// The intrinsic extent of a glyph run, so a caption leaf sizes to it.
fn glyph_run_natural(glyphs: &[GlyphInstanceData]) -> Vec2 {
    let mut n = Vec2::ZERO;
    for g in glyphs {
        n.x = n.x.max(g.rect.x + g.rect.w);
        n.y = n.y.max(g.rect.y + g.rect.h);
    }
    n
}

/// Walk a node's direct children (arena sibling chain).
fn children_of(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
    let arena = store.arena();
    let mut out = Vec::new();
    for c in arena.children(parent) {
        out.push(c);
    }
    out
}

/// The tab buttons: the strip viewport's row's children.
fn buttons_of(store: &NodeStore, root: NodeId) -> Vec<NodeId> {
    let viewport = children_of(store, root)[0];
    let row = children_of(store, viewport)[0];
    children_of(store, row)
}

/// Replace each tab caption's content with a shared deterministic glyph run, so
/// the golden is font/atlas-stable without a real font stack and the atlas stops
/// growing after build — the same fixture bypass `button_widget.rs` uses. Each
/// tab button's first child is its caption leaf (`Label` maps to one leaf).
fn attach_captions(gpu: &mut CpuRaster, store: &mut NodeStore, root: NodeId) {
    let tg = test_glyphs([0.0, 0.0], 26.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "tabs-caption-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);
    let natural = glyph_run_natural(&tg.glyphs);

    for button in buttons_of(store, root) {
        let caption = children_of(store, button)[0];
        store.set_content_payload(
            caption,
            Content::Text {
                ink: None,
                glyphs: tg.glyphs.clone(),
                atlas,
                lane: viso::render::GlyphLane::CoverageA8,
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: WHITE,
                natural,
                baseline: 0.0,
                shaped_at_width: None,
                width_use: viso::ui::WidthUse::Ignored,
                min_content: natural.x,
                max_content: natural.x,
            },
        );
    }
}

// --- golden screenshot ------------------------------------------------------

#[test]
fn tabs_render_the_strip_and_selected_panel_and_match_golden() {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let root = build_scene(&mut store);
    attach_captions(&mut gpu, &mut store, root);

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/tabs_widget.bgra8")
}

// --- allocation profile ---------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `splitter_widget.rs`.
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
    attach_captions(&mut gpu, &mut store, root);

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
fn steady_tabs_frame_is_allocation_free_including_after_a_tab_flip() {
    let mut h = setup_alloc();

    // Flip the selected tab once through a direct `set_hidden` pair (the same effect
    // the router applies for a click): hide panel 0, show panel 1. The flip marks
    // LAYOUT | PAINT, so re-run layout to settle the new visibility before measuring
    // — the one-time flip/layout cost folds into warmup.
    let area = children_of(&h.store, h.root)[1];
    let panels = children_of(&h.store, area);
    h.store.set_hidden(panels[0], true);
    h.store.set_hidden(panels[1], false);
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
            "frame {i}: frame_stats changed for an unchanged Tabs scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Tabs scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Tabs scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Tabs scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a Tabs frame allocated a different amount on two identical steady frames \
         ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
        ..
    } = stats;
    assert!(draw_calls > 0, "the Tabs scene must emit draw calls");
    assert!(instances > 0, "the Tabs scene must emit instances");
}
