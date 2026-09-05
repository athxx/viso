//! Section 71 validation pack for the `RadioGroup` control — a single-select
//! group of options — driven through the public facade. Mirrors
//! `toggle_widget.rs` (golden + a11y + allocation + the two interactive input
//! tapes) with the group's own structure: a `Column` of focusable option rows,
//! each a circular *dot* leaf followed by a composed `Label` caption. A primary
//! click and a keyboard activation on a row both drive the same `on_change` and
//! write the row's index into one shared selection cell — mutual exclusion is
//! structural (one `Int` cell bound to every row's `PAINT`).
//!
//! - **golden screenshot + measure** — build a `RadioGroup` with
//!   `RadioGroup::build` (a column of `dot + Label` rows), attach a deterministic
//!   glyph run to each caption leaf (the facade `TextShaper` is `pub(crate)`, so
//!   integration tests shape via the shared `viso-render` fixtures rather than a
//!   real font stack), lay it out, and confirm the pixels match a blessed
//!   baseline;
//! - **pointer input tape** — `PointerRouter::route` over a row's world box: a
//!   primary press-then-release selects that option and fires `on_change` once
//!   with its index; selecting a second option reports the new index; a
//!   non-primary button does not select;
//! - **keyboard input tape** — with a row focused, `KeyRouter::route_key` selects
//!   on Enter and Space; an auto-repeat does not, and an unfocused group receives
//!   no key dispatch;
//! - **a11y snapshot** — a `RadioGroup`'s derived semantics root is `Role::Group`
//!   and each option is a `Role::CheckBox` named by its caption;
//! - **allocation profile** — a warmed-up `RadioGroup` frame allocates nothing
//!   per frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `toggle_widget.rs`.
//!
//! Building through `RadioGroup::build` is the point: it proves the widget lowers
//! to an interactive, content-bearing subtree the `viso-ui` input/paint path and
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
use viso::widgets::{ViewStyle, radio_group, view};

const W: u32 = 160;
const H: u32 = 96;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

const OPTIONS: [&str; 3] = ["Small", "Medium", "Large"];

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

/// Build a padded dark `View` wrapping a single `RadioGroup`, and return the
/// root, the group's node, and each option row's caption leaf. The group is the
/// container's only child, so its node is the root's first child; within the
/// group, the option rows are the children in order, and within each row the dot
/// leaf is the *first* child and the caption `Label` leaf the *second*. Wrapping
/// in a fill container keeps the group at its `Fit` intrinsic size rather than
/// filling the surface.
fn build_scene(store: &mut NodeStore) -> (NodeId, NodeId, Vec<NodeId>) {
    // The group authors a shared selection cell, so it must build through a
    // reactive cx; the golden only paints (the routers are exercised
    // separately), so throwaway state stores are fine here.
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
        radio_group(OPTIONS).build(cx);
    });

    let root = {
        let mut cx = BuildCx::with_reactive(store, &mut states, &mut bindings, &mut lists);
        container.build(&mut cx);
        cx.root().expect("scene has a root")
    };
    let group_id = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .expect("the view has the group as its only child");

    // Walk the group's option rows in order; each row's caption leaf is the
    // second child (after the dot leaf).
    let mut captions = Vec::new();
    let mut row = store.arena().links(group_id).and_then(|l| l.first_child);
    while let Some(r) = row {
        let caption = store
            .arena()
            .links(r)
            .and_then(|l| l.first_child)
            .and_then(|dot| store.arena().links(dot).and_then(|l| l.next_sibling))
            .expect("each option row has a caption leaf as its second child");
        captions.push(caption);
        row = store.arena().links(r).and_then(|l| l.next_sibling);
    }
    (root, group_id, captions)
}

/// Attach the shared deterministic glyph run to each caption leaf so the golden
/// is font/atlas-stable without a real font stack — the same fixture bypass
/// `toggle_widget.rs` uses. All captions share one atlas.
fn attach_captions(gpu: &mut HeadlessRaster, store: &mut NodeStore, captions: &[NodeId]) {
    let tg = test_glyphs([0.0, 0.0], 18.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "radio-caption-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);

    let natural = glyph_run_natural(&tg.glyphs);
    for &caption in captions {
        store.set_content_payload(
            caption,
            Content::Text {
                glyphs: tg.glyphs.clone(),
                atlas,
                color: WHITE,
                natural,
            },
        );
    }
}

// --- golden screenshot + measure -------------------------------------------

#[test]
fn radio_group_renders_dots_and_captions_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let (root, _group_id, captions) = build_scene(&mut store);
    attach_captions(&mut gpu, &mut store, &captions);

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/radio_widget.bgra8")
}

// --- interactive input tapes ------------------------------------------------

/// A `RadioGroup` laid out as the root of its own tree, kept together with the
/// reactive stores its handlers write into so a router can drive it. The group
/// fills the surface (group `Fill`) so a pointer sample aimed at a row hits it.
/// Its `on_change` records the last selected index and bumps a counter.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    group: NodeId,
    rows: Vec<NodeId>,
    chain: Vec<NodeId>,
}

impl Interactive {
    /// Build a fill group whose `on_change` records the selected index and bumps
    /// `counter`, laid out over the surface.
    fn new(counter: Rc<Cell<u32>>, last: Rc<Cell<Option<usize>>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();

        let widget = radio_group(OPTIONS)
            .size(Size::fill())
            .on_change(move |_ev, index| {
                last.set(Some(index));
                counter.set(counter.get() + 1);
            });

        let group = {
            let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
            widget.build(&mut cx);
            cx.root().expect("group declares a root")
        };

        let mut scratch = Vec::new();
        store.layout(group, surface_rect(), &mut scratch);

        // Collect the option rows in order for targeting.
        let mut rows = Vec::new();
        let mut row = store.arena().links(group).and_then(|l| l.first_child);
        while let Some(r) = row {
            rows.push(r);
            row = store.arena().links(r).and_then(|l| l.next_sibling);
        }

        Interactive {
            store,
            states,
            bindings,
            group,
            rows,
            chain: Vec::new(),
        }
    }

    /// The center of a row's laid-out world box — a pointer sample there hits it.
    fn row_center(&self, index: usize) -> (f32, f32) {
        let b = self.store.bounds(self.rows[index]);
        (b.x + b.w / 2.0, b.y + b.h / 2.0)
    }

    /// Route a pointer sample through the public `PointerRouter`, exactly as the
    /// facade's `on_input` does. Returns whether any handler ran.
    fn pointer(&mut self, ev: PointerEvent) -> bool {
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.group,
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
            self.group,
            ev,
            &mut self.chain,
        )
    }
}

/// A primary-button pointer sample at `(x, y)` in the given phase.
fn primary_at(x: f32, y: f32, phase: PointerPhase) -> PointerEvent {
    PointerEvent {
        x,
        y,
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
fn pointer_click_selects_option_and_fires_change() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<usize>));
    let mut ix = Interactive::new(count.clone(), last.clone());

    let (x1, y1) = ix.row_center(1);
    assert!(
        ix.pointer(primary_at(x1, y1, PointerPhase::Down)),
        "the press hits the option's handler"
    );
    assert_eq!(count.get(), 0, "the press alone does not select");

    assert!(ix.pointer(primary_at(x1, y1, PointerPhase::Up)));
    assert_eq!(count.get(), 1, "press-then-release is one selection");
    assert_eq!(last.get(), Some(1), "on_change carries the selected index");

    let (x2, y2) = ix.row_center(2);
    ix.pointer(primary_at(x2, y2, PointerPhase::Down));
    ix.pointer(primary_at(x2, y2, PointerPhase::Up));
    assert_eq!(count.get(), 2, "selecting another option fires again");
    assert_eq!(last.get(), Some(2), "and reports the new index");
}

#[test]
fn non_primary_pointer_does_not_select() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<usize>));
    let mut ix = Interactive::new(count.clone(), last);

    let (x, y) = ix.row_center(0);
    let down = PointerEvent {
        buttons: PointerButtons::NONE,
        ..primary_at(x, y, PointerPhase::Down)
    };
    let up = PointerEvent {
        buttons: PointerButtons::NONE,
        ..primary_at(x, y, PointerPhase::Up)
    };
    ix.pointer(down);
    ix.pointer(up);
    assert_eq!(count.get(), 0, "a non-primary button does not select");
}

#[test]
fn keyboard_select_requires_focus_and_ignores_repeat() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<usize>));
    let mut ix = Interactive::new(count.clone(), last.clone());

    // Unfocused: the key router has no target, so nothing fires.
    assert!(
        !ix.key(key_ev(Key::Enter, true, false)),
        "an unfocused group receives no key dispatch"
    );
    assert_eq!(count.get(), 0);

    // Enter on a focused row selects it (0 -> 2 is a genuine change).
    ix.store.set_focused(Some(ix.rows[2]));
    assert!(ix.key(key_ev(Key::Enter, true, false)));
    assert_eq!(last.get(), Some(2), "Enter selects the focused option");
    assert_eq!(count.get(), 1);

    // Space on a *different* focused row selects it (2 -> 1 change); Space is the
    // accessibility equivalent of Enter.
    ix.store.set_focused(Some(ix.rows[1]));
    assert!(ix.key(key_ev(Key::Space, true, false)));
    assert_eq!(last.get(), Some(1), "Space selects the focused option");
    assert_eq!(count.get(), 2);

    // Re-selecting the already-selected option is a no-op (no change, no fire).
    ix.key(key_ev(Key::Enter, true, false));
    assert_eq!(
        count.get(),
        2,
        "re-selecting the current option does not fire"
    );

    ix.key(key_ev(Key::Enter, true, true)); // auto-repeat: ignored
    ix.key(key_ev(Key::Enter, false, false)); // key-up: ignored
    assert_eq!(count.get(), 2, "auto-repeat and key-up do not select");
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn radio_group_derives_a_group_over_named_checkbox_options() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();

    let root = {
        let widget = radio_group(OPTIONS);
        let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
        widget.build(&mut cx);
        cx.root().expect("group declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(
        node.role,
        Role::Group,
        "a RadioGroup derives Role::Group as its accessible wrapper"
    );

    // `children` holds indices into the flat pre-order `tree.nodes`.
    let labels: Vec<Option<&str>> = node
        .children
        .iter()
        .map(|&i| {
            let c = &tree.nodes[i];
            assert_eq!(
                c.role,
                Role::CheckBox,
                "each option derives Role::CheckBox (no radio variant yet)"
            );
            c.label.as_deref()
        })
        .collect();
    assert_eq!(
        labels,
        vec![Some("Small"), Some("Medium"), Some("Large")],
        "each option's accessible name is its visible caption, in order"
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
    let (root, _group_id, captions) = build_scene(&mut store);
    attach_captions(&mut gpu, &mut store, &captions);

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
fn steady_radio_frame_is_allocation_free() {
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
            "frame {i}: frame_stats changed for an unchanged RadioGroup scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged RadioGroup scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged RadioGroup scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged RadioGroup scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a RadioGroup frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the RadioGroup scene must emit draw calls");
    assert!(instances > 0, "the RadioGroup scene must emit instances");
}
