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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::{FrameStats, GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs};
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, Component, Content, Inset, Key, KeyEvent, KeyRouter,
    Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Role,
    Size, StateStore, Vec2, VirtualLists, paint_tree,
};
use viso::widgets::{ViewStyle, button, view};

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
        let mut cx = BuildCx::with_reactive(store, &mut states, &mut bindings, &mut lists);
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
            color: WHITE,
            natural,
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

        let widget = button("OK")
            .size(Size::fill())
            .on_click(move |_ev| counter.set(counter.get() + 1));

        let button = {
            let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
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

    let root = {
        let widget = button("OK");
        let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
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
