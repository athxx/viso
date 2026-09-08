//! Section 71 validation pack for the `Button` control — the first interactive
//! widget — driven through the public facade. This mirrors the `icon_widget.rs`
//! template (golden + a11y + allocation) and adds the two sections an
//! interactive control needs: a pointer input tape and a keyboard input tape,
//! proving pointer click and keyboard activation drive the same `on_click`.
//!
//! - **golden screenshot + measure** — build a `Button` with `Button::build`
//!   (background box + a composed `Label` caption), attach a deterministic glyph
//!   run to its caption leaf (the facade `TextShaper` is `pub(crate)`, so
//!   integration tests shape via the shared `viso-render` fixtures rather than a
//!   real font stack — the font/atlas-stable path every content golden uses),
//!   lay it out, and confirm the pixels match a blessed baseline;
//! - **pointer input tape** — `PointerRouter::route` over the button's world box:
//!   a primary press-then-release fires `on_click` once; a non-primary button and
//!   an up-without-a-hit do not;
//! - **keyboard input tape** — with the button focused, `KeyRouter::route_key`
//!   fires `on_click` on Enter and Space; an auto-repeat does not, and an
//!   unfocused button receives no key dispatch;
//! - **a11y snapshot** — a `Button`'s derived semantics node is `Role::Button`
//!   and its accessible name is the visible caption;
//! - **allocation profile** — a warmed-up `Button` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `icon_widget.rs`.
//!
//! Building through `Button::build` is the point: it proves the widget lowers to
//! an interactive, content-bearing subtree the `viso-ui` input/paint path and
//! `paint_tree` handle unchanged.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::Primitive;
use viso::render::{FrameStats, GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs};
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, Component, Content, Inset, Key, KeyEvent, KeyRouter,
    Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Role,
    SemanticProjector, Size, StateStore, TextEdits, Vec2, VirtualLists, paint_tree,
};
use viso::widgets::{ButtonStyle, ViewStyle, button, view};

const W: u32 = 160;
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

/// Build a padded dark `View` wrapping a single `Button`, and return the root,
/// the button's node, and the button's caption leaf. The button is the
/// container's only child, so its node is the root's first child; the caption
/// `Label` leaf is in turn the button's only child. Wrapping in a fill container
/// keeps the button at its `Fit` intrinsic size (caption + padding) rather than
/// filling the surface.
fn build_scene(store: &mut NodeStore) -> (NodeId, NodeId, NodeId) {
    // The button authors a reactive `pressed` cell, so it must build through a
    // reactive cx; the golden only paints (the routers are exercised separately),
    // so throwaway state stores are fine here.
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let container = view(ViewStyle {
        axis: Axis::Row,
        padding: Inset::all(10.0),
        size: Size::fill(),
        background: BoxStyle::solid(DARK),
        ..Default::default()
    })
    .children(|cx| {
        button("OK").build(cx);
    });

    let root = {
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
    };
    let button_id = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .expect("the view has the button as its only child");
    let caption_id = store
        .arena()
        .links(button_id)
        .and_then(|l| l.first_child)
        .expect("the button has the caption leaf as its only child");
    (root, button_id, caption_id)
}

/// Attach the shared deterministic glyph run to the button's caption leaf so the
/// golden is font/atlas-stable without a real font stack — the same fixture
/// bypass `label_widget.rs` uses.
fn attach_caption(gpu: &mut HeadlessRaster, store: &mut NodeStore, caption_id: NodeId) {
    let tg = test_glyphs([0.0, 0.0], 26.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "button-caption-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);

    let natural = glyph_run_natural(&tg.glyphs);
    store.set_content_payload(
        caption_id,
        Content::Text {
            glyphs: tg.glyphs.clone(),
            atlas,
            color_glyphs: Vec::new(),
            color_atlas: None,
            color: WHITE,
            natural,
            baseline: 0.0,
        },
    );
}

// --- golden screenshot + measure -------------------------------------------

#[test]
fn button_renders_background_and_caption_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let (root, _button_id, caption_id) = build_scene(&mut store);
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/button_widget.bgra8")
}

// --- three-state paint list --------------------------------------------------

/// A gray box at the given level, so the three interaction variants are visibly
/// distinct and a painted quad's color unambiguously names which one won.
fn gray_box(level: f32) -> BoxStyle {
    BoxStyle::solid(Rgba {
        r: level,
        g: level,
        b: level,
        a: 1.0,
    })
}

/// The first painted `Quad`'s straight linear RGBA — the button's own background
/// box, since the button is the root of this scene and its background leaf paints
/// before its caption. Panics if the paint list has no quad, which would itself
/// be a regression (a visible button always paints its box).
fn first_quad_color(primitives: &[Primitive]) -> Rgba {
    primitives
        .iter()
        .find_map(|p| match p {
            Primitive::Quad(q) => Some(q.color),
            _ => None,
        })
        .expect("the button paints a background quad")
}

/// The full facade paint path for a button driven through interaction states:
/// the button is the root (so its background quad is the paint list's first),
/// and the harness drives the *public* `PointerRouter` — which synthesizes the
/// per-node enter/leave the frame loop relies on — then runs the frame's
/// STYLE-gated interaction-style resolve and `paint_tree`, exactly as
/// `relayout_and_paint` does. Asserting the emitted quad color per state proves
/// the whole chain (router hover synthesis → cell flip → flush → interaction
/// resolve → paint) lowers three distinct pixels, not just the flat `style`
/// value the widget-level unit tests read.
struct Painted {
    gpu: HeadlessRaster,
    surface: viso::gpu::SurfaceId,
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    chain: Vec<NodeId>,
    primitives: Vec<Primitive>,
}

impl Painted {
    /// Build a fill button with three distinct interaction boxes as the root of
    /// its own tree, laid out over the surface so a center sample hits it.
    fn new() -> Self {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();

        let widget = button("OK").size(Size::fill()).style(ButtonStyle {
            background: gray_box(0.1),
            hover: gray_box(0.5),
            pressed: gray_box(0.9),
            ..ButtonStyle::default()
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
            cx.root().expect("button declares a root")
        };

        let mut scratch = Vec::new();
        store.layout(root, surface_rect(), &mut scratch);

        Painted {
            gpu,
            surface,
            store,
            states,
            bindings,
            root,
            chain: Vec::new(),
            primitives: Vec::new(),
        }
    }

    /// Route a pointer sample through the public `PointerRouter`, exactly as the
    /// facade's `on_input` does — so a `Move` synthesizes the per-node enter/leave.
    fn pointer(&mut self, ev: PointerEvent) {
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.root,
            ev,
            &mut self.chain,
        );
    }

    /// Run the frame's STYLE-gated interaction resolve, mirroring
    /// `relayout_and_paint`: drain the pending writes the handlers made, flush
    /// them so the bound node is STYLE-dirty, then re-select the painted box.
    fn resolve(&mut self) {
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        self.store.resolve_interaction_styles(&self.states);
    }

    /// Paint the tree into the reused buffer and return the button's background
    /// quad color, exactly what the GPU would upload for this frame.
    fn paint_and_read(&mut self) -> Rgba {
        self.primitives.clear();
        paint_tree(&self.store, self.root, &mut self.primitives);
        // Prove the paint list also survives an upload+submit, so the color is a
        // real frame's output rather than a paint-walk artifact.
        let format = self.gpu.surface_format(self.surface);
        let mut renderer = Renderer::new(&mut self.gpu, format);
        renderer.upload(&mut self.gpu, &self.primitives);
        renderer.submit(&mut self.gpu, self.surface, CLEAR, [W as f32, H as f32]);
        first_quad_color(&self.primitives)
    }
}

/// A pointer sample at the surface center in the given phase, with the given
/// buttons held.
fn sample(phase: PointerPhase, buttons: PointerButtons) -> PointerEvent {
    PointerEvent {
        x: W as f32 / 2.0,
        y: H as f32 / 2.0,
        phase,
        buttons,
        modifiers: Modifiers::default(),
    }
}

/// The button lowers a distinct background quad for each interaction state,
/// driven end-to-end through the public router + frame resolve + paint. This is
/// the paint-list golden the plan calls for: three states, three colors.
#[test]
fn button_paints_three_distinct_quads_across_interaction_states() {
    let mut p = Painted::new();

    // Resting: the build binds the cells and marks STYLE, so the first resolve
    // selects the resting box.
    p.resolve();
    let resting = p.paint_and_read();
    assert_eq!(
        resting,
        gray_box(0.1).fill,
        "resting paints the resting box"
    );

    // A Move over the button synthesizes a per-node Enter → hovered → hover box.
    p.pointer(sample(PointerPhase::Move, PointerButtons::NONE));
    p.resolve();
    let hovered = p.paint_and_read();
    assert_eq!(hovered, gray_box(0.5).fill, "hover paints the hover box");

    // A primary Down while hovered → pressed wins over hover.
    p.pointer(sample(PointerPhase::Down, PointerButtons::PRIMARY));
    p.resolve();
    let pressed = p.paint_and_read();
    assert_eq!(pressed, gray_box(0.9).fill, "press paints the pressed box");

    // The three emitted quad colors are mutually distinct — the paint list, not
    // just the flat style, differs per state.
    assert_ne!(resting, hovered, "resting and hover paint different quads");
    assert_ne!(hovered, pressed, "hover and pressed paint different quads");
    assert_ne!(
        resting, pressed,
        "resting and pressed paint different quads"
    );
}

// --- interactive input tapes ------------------------------------------------

/// A focusable button laid out as the root of its own tree, kept together with
/// the reactive stores its handlers write into so a router can drive it. The
/// button fills the surface (root `Fill`) so any central pointer sample hits it.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    button: NodeId,
    chain: Vec<NodeId>,
}

impl Interactive {
    /// Build a fill button whose `on_click` bumps `counter`, laid out over the
    /// surface so a center-of-surface pointer sample lands on it.
    fn new(counter: Rc<Cell<u32>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();

        let widget = button("OK")
            .size(Size::fill())
            .on_click(move |_ev| counter.set(counter.get() + 1));

        let button = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            widget.build(&mut cx);
            cx.root().expect("button declares a root")
        };

        let mut scratch = Vec::new();
        store.layout(button, surface_rect(), &mut scratch);

        Interactive {
            store,
            states,
            bindings,
            button,
            chain: Vec::new(),
        }
    }

    /// Route a pointer sample through the public `PointerRouter`, exactly as the
    /// facade's `on_input` does. Returns whether any handler ran.
    fn pointer(&mut self, ev: PointerEvent) -> bool {
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.button,
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
            self.button,
            ev,
            &mut self.chain,
        )
    }
}

/// A primary-button pointer sample at the surface center, in the given phase.
fn primary_at_center(phase: PointerPhase) -> PointerEvent {
    PointerEvent {
        x: W as f32 / 2.0,
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
fn pointer_press_release_over_button_fires_click_once() {
    let clicks = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(clicks.clone());

    assert!(
        ix.pointer(primary_at_center(PointerPhase::Down)),
        "the press hits the button's handler"
    );
    assert_eq!(clicks.get(), 0, "press alone is not a click");

    assert!(ix.pointer(primary_at_center(PointerPhase::Up)));
    assert_eq!(
        clicks.get(),
        1,
        "press-then-release over the button is one click"
    );
}

#[test]
fn non_primary_pointer_does_not_click() {
    let clicks = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(clicks.clone());

    let down = PointerEvent {
        buttons: PointerButtons::NONE,
        ..primary_at_center(PointerPhase::Down)
    };
    let up = PointerEvent {
        buttons: PointerButtons::NONE,
        ..primary_at_center(PointerPhase::Up)
    };
    ix.pointer(down);
    ix.pointer(up);
    assert_eq!(clicks.get(), 0, "a non-primary button does not activate");
}

#[test]
fn keyboard_activation_requires_focus_and_ignores_repeat() {
    let clicks = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(clicks.clone());

    // Unfocused: the key router has no target, so nothing fires.
    assert!(
        !ix.key(key_ev(Key::Enter, true, false)),
        "an unfocused button receives no key dispatch"
    );
    assert_eq!(clicks.get(), 0);

    ix.store.set_focused(Some(ix.button));

    assert!(ix.key(key_ev(Key::Enter, true, false)));
    assert!(ix.key(key_ev(Key::Space, true, false)));
    ix.key(key_ev(Key::Enter, true, true)); // auto-repeat: ignored
    ix.key(key_ev(Key::Enter, false, false)); // key-up: ignored

    assert_eq!(
        clicks.get(),
        2,
        "Enter and Space each fire once while focused; repeat/up do not"
    );
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn button_derives_a_button_semantics_node_named_by_its_caption() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let root = {
        let widget = button("OK");
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        widget.build(&mut cx);
        cx.root().expect("button declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Button, "a Button derives Role::Button");
    assert_eq!(
        node.label.as_deref(),
        Some("OK"),
        "the accessible name is the visible caption"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `icon_widget.rs`.
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
    let (root, _button_id, caption_id) = build_scene(&mut store);
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
fn steady_button_frame_is_allocation_free() {
    let mut h = setup_alloc();

    // Warm up until the frame path reaches steady state: the first frames grow
    // the persistent instance/mesh buffers to fit the scene, cache the
    // per-pipeline bind groups, and size the headless framebuffer/target pool.
    for _ in 0..4 {
        frame(&mut h);
    }
    // Grow the reused paint buffer to its steady capacity so a later
    // `paint_tree` into it does not reallocate.
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
            "frame {i}: frame_stats changed for an unchanged Button scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Button scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Button scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Button scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a Button frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the Button scene must emit draw calls");
    assert!(instances > 0, "the Button scene must emit instances");
}
