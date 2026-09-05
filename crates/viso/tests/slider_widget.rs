//! Section 71 validation pack for the `Slider` control — a draggable thumb on a
//! track that picks a value from a `[min, max]` range — driven through the public
//! facade. Mirrors `toggle_widget.rs` (golden + a11y + allocation + the two
//! interactive input tapes) with the slider's own structure: a flex row holding a
//! rounded *track* (which holds a circular *thumb* leaf) plus a composed `Label`
//! caption. A primary *drag* and an *arrow-key* step drive the same `on_change`
//! and move the reactive relative-value cell.
//!
//! - **golden screenshot + measure** — build a `Slider` with `Slider::build`
//!   (a row holding a track flex + thumb leaf, then a composed `Label` caption),
//!   attach a deterministic glyph run to its caption leaf (the facade `TextShaper`
//!   is `pub(crate)`, so integration tests shape via the shared `viso-render`
//!   fixtures rather than a real font stack), lay it out, and confirm the pixels
//!   match a blessed baseline;
//! - **pointer drag input tape** — `PointerRouter::route` over the slider's world
//!   box: a primary press *captures the pointer to the slider* (so subsequent
//!   samples route to it even outside its box), a move drags the value by the
//!   pixel delta over the track width and fires `on_change`, and the release frees
//!   the capture;
//! - **keyboard step input tape** — with the slider focused, `KeyRouter::route_key`
//!   steps the value on the arrow keys (Right/Up up, Left/Down down) and fires
//!   `on_change`; an unfocused slider receives no key dispatch;
//! - **a11y snapshot** — a `Slider`'s derived semantics node is `Role::CheckBox`
//!   (a dedicated `Slider` role is a later slice) and its accessible name is the
//!   visible caption;
//! - **allocation profile** — a warmed-up `Slider` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `toggle_widget.rs`.
//!
//! Building through `Slider::build` is the point: it proves the widget lowers to
//! an interactive, content-bearing subtree the `viso-ui` input/paint path and
//! `paint_tree` handle unchanged — including pointer *capture*, which a drag needs
//! but a click does not.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::{FrameStats, GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs};
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, Component, Content, Inset, Key, KeyEvent, KeyRouter,
    Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Role,
    Size, StateStore, Vec2, VirtualLists, paint_tree,
};
use viso::widgets::{ViewStyle, slider, view};

const W: u32 = 200;
const H: u32 = 96;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

const DARK: Rgba = Rgba {
    r: 0.1,
    g: 0.1,
    b: 0.12,
    a: 1.0,
};
const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// The intrinsic extent of the glyph run, so the caption leaf sizes to it.
fn glyph_run_natural(glyphs: &[GlyphInstanceData]) -> Vec2 {
    let mut n = Vec2::ZERO;
    for g in glyphs {
        n.x = n.x.max(g.rect.x + g.rect.w);
        n.y = n.y.max(g.rect.y + g.rect.h);
    }
    n
}

fn surface_rect() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    }
}

/// Build a padded dark `View` wrapping a single `Slider`, and return the root, the
/// slider's node, and the slider's caption leaf. The slider is the container's
/// only child, so its node is the root's first child; within the slider, the track
/// flex is the *first* child (it holds the thumb) and the caption `Label` leaf is
/// the *second*. Wrapping in a fill container keeps the slider at its `Fit`
/// intrinsic size (track + gap + caption) rather than filling the surface.
fn build_scene(store: &mut NodeStore) -> (NodeId, NodeId, NodeId) {
    // The slider authors reactive cells, so it must build through a reactive cx;
    // the golden only paints (the routers are exercised separately), so throwaway
    // state stores are fine here.
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();

    let container = view(ViewStyle {
        axis: Axis::Row,
        padding: Inset::all(10.0),
        size: Size::fill(),
        background: BoxStyle::solid(DARK),
        ..Default::default()
    })
    .children(|cx| {
        slider("Volume").range(0.0, 100.0).value(50.0).build(cx);
    });

    let root = {
        let mut cx = BuildCx::with_reactive(store, &mut states, &mut bindings, &mut lists);
        container.build(&mut cx);
        cx.root().expect("scene has a root")
    };
    let slider_id = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .expect("the view has the slider as its only child");
    // The track flex is the first child (it holds the thumb); the caption `Label`
    // leaf is the second child.
    let track_id = store
        .arena()
        .links(slider_id)
        .and_then(|l| l.first_child)
        .expect("the slider has a track flex as its first child");
    let caption_id = store
        .arena()
        .links(track_id)
        .and_then(|l| l.next_sibling)
        .expect("the slider has the caption leaf as its second child");
    (root, slider_id, caption_id)
}

/// Attach the shared deterministic glyph run to the slider's caption leaf so the
/// golden is font/atlas-stable without a real font stack — the same fixture
/// bypass `toggle_widget.rs` uses.
fn attach_caption(gpu: &mut HeadlessRaster, store: &mut NodeStore, caption_id: NodeId) {
    let tg = test_glyphs([0.0, 0.0], 26.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "slider-caption-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);

    let natural = glyph_run_natural(&tg.glyphs);
    store.set_content_payload(
        caption_id,
        Content::Text {
            glyphs: tg.glyphs.clone(),
            atlas,
            color: WHITE,
            natural,
        },
    );
}

// --- golden screenshot + measure -------------------------------------------

#[test]
fn slider_renders_track_thumb_and_caption_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let (root, _slider_id, caption_id) = build_scene(&mut store);
    attach_caption(&mut gpu, &mut store, caption_id);

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/slider_widget.bgra8")
}

// --- interactive input tapes ------------------------------------------------

/// A focusable slider laid out as the root of its own tree, kept together with the
/// reactive stores its handlers write into so a router can drive it. The slider
/// fills the surface (root `Fill`) so any central pointer sample hits it. Its
/// `on_change` records the last carried value and bumps a counter.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    slider: NodeId,
    chain: Vec<NodeId>,
}

impl Interactive {
    /// Build a fill slider whose `on_change` records the new value and bumps
    /// `counter`, laid out over the surface so a center-of-surface pointer sample
    /// lands on it. The `[0, 100]` range with the default 120px track keeps the
    /// drag arithmetic (`dx / track_width`) easy to check.
    fn new(counter: Rc<Cell<u32>>, last: Rc<Cell<Option<f32>>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();

        let widget = slider("Volume")
            .range(0.0, 100.0)
            .size(Size::fill())
            .on_change(move |_ev, v| {
                last.set(Some(v));
                counter.set(counter.get() + 1);
            });

        let slider = {
            let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
            widget.build(&mut cx);
            cx.root().expect("slider declares a root")
        };

        let mut scratch = Vec::new();
        store.layout(slider, surface_rect(), &mut scratch);

        Interactive {
            store,
            states,
            bindings,
            slider,
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
            self.slider,
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
            self.slider,
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
fn pointer_drag_moves_value_captures_and_fires_change() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<f32>));
    let mut ix = Interactive::new(count.clone(), last.clone());

    // A primary press near the left of the surface anchors the drag and captures
    // the pointer to the slider — the router applies the request, so subsequent
    // samples route to the slider even outside its box.
    assert!(
        ix.pointer(primary_at(20.0, PointerPhase::Down)),
        "the press hits the slider's handler"
    );
    assert_eq!(
        ix.store.capture(),
        Some(ix.slider),
        "the press captures the pointer to the slider so a drag keeps tracking"
    );
    assert_eq!(count.get(), 0, "the press alone does not fire a change");

    // Move right by 60px over the default 120px track: +0.5 relative. The slider
    // started at min (0), so the external value lands at 50 of [0, 100].
    assert!(ix.pointer(primary_at(80.0, PointerPhase::Move)));
    assert_eq!(count.get(), 1, "the move fires one change");
    assert_eq!(
        last.get(),
        Some(50.0),
        "on_change carries the external value dragged to mid-range"
    );

    // A sample far to the right of the surface still routes here because the
    // pointer is captured; it clamps the value to the far end (100).
    assert!(ix.pointer(primary_at(1000.0, PointerPhase::Move)));
    assert_eq!(
        last.get(),
        Some(100.0),
        "a captured drag past the track clamps to the max"
    );

    // The release frees the capture.
    ix.pointer(primary_at(1000.0, PointerPhase::Up));
    assert_eq!(
        ix.store.capture(),
        None,
        "the release frees the pointer capture"
    );
}

#[test]
fn keyboard_step_requires_focus_and_moves_the_value() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<f32>));
    let mut ix = Interactive::new(count.clone(), last.clone());

    // Unfocused: the key router has no target, so nothing fires.
    assert!(
        !ix.key(key_ev(Key::Right, true, false)),
        "an unfocused slider receives no key dispatch"
    );
    assert_eq!(count.get(), 0);

    ix.store.set_focused(Some(ix.slider));

    // The slider starts at min (0). A continuous [0, 100] slider steps 5% of the
    // range per arrow press = 5 units.
    assert!(ix.key(key_ev(Key::Right, true, false)));
    assert_eq!(last.get(), Some(5.0), "Right steps up 5% of the range");
    assert!(ix.key(key_ev(Key::Up, true, false)));
    assert_eq!(last.get(), Some(10.0), "Up steps up too");
    assert!(ix.key(key_ev(Key::Left, true, false)));
    assert_eq!(last.get(), Some(5.0), "Left steps back down");

    ix.key(key_ev(Key::Right, false, false)); // key-up: ignored
    assert_eq!(
        count.get(),
        3,
        "each arrow press fired one change; the key-up did not"
    );
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn slider_derives_a_checkbox_semantics_node_named_by_its_caption() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();

    let root = {
        let widget = slider("Volume");
        let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
        widget.build(&mut cx);
        cx.root().expect("slider declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(
        node.role,
        Role::CheckBox,
        "a Slider derives Role::CheckBox for now (a dedicated Slider role is a later slice)"
    );
    assert_eq!(
        node.label.as_deref(),
        Some("Volume"),
        "the accessible name is the visible caption"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `toggle_widget.rs`.
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
    let (root, _slider_id, caption_id) = build_scene(&mut store);
    attach_caption(&mut gpu, &mut store, caption_id);

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
    } = stats;
    assert!(draw_calls > 0, "the Slider scene must emit draw calls");
    assert!(instances > 0, "the Slider scene must emit instances");
}
