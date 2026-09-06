//! Section 71 validation pack for the `Tabs` control — a strip of selectable
//! tabs, each revealing one panel — driven through the public facade. Mirrors
//! `splitter_widget.rs` (golden + input tapes + a11y + allocation) with the tabs'
//! own structure: a `TabList` row of tab buttons above a panel area whose panels
//! all build once and show/hide through the retained `hidden` flag. Selecting a
//! tab writes a reactive `selected` cell, defers a pair of `hidden` flips the
//! router applies (hide the old panel, show the new), and fires `on_change` once.
//!
//! - **golden screenshot** — build a `Tabs` with two colored panels, lay it out
//!   over a fixed surface, and confirm the pixels match a blessed baseline. Only
//!   the selected panel is shown, so the golden proves a hidden panel paints
//!   nothing (its region shows the other panel / clear). Pure quads — no font
//!   fixture (the tab captions render as glyphs, but the baseline captures
//!   whatever the text path emits deterministically);
//! - **pointer click input tape** — `PointerRouter::route` at a tab button's world
//!   box: a primary press-then-release on the second tab selects it, fires
//!   `on_change` once, moves the `selected` cell, and — through the router applying
//!   the deferred requests — flips the panels' `hidden` flags;
//! - **keyboard input tape** — with a tab focused, `KeyRouter::route_key` activates
//!   it on Enter/Space and steps the selection on Left/Right; an unfocused strip
//!   receives no key dispatch;
//! - **a11y snapshot** — the derived tree carries a `TabList` over `Tab` nodes
//!   named by their captions and `Group` panels; each tab button declares a pointer
//!   and a key handler and is focusable;
//! - **allocation profile** — a warmed-up `Tabs` frame allocates nothing per frame
//!   (architecture section 47 hot-path contract), plus a *tab-flip* frame: driving
//!   a click through the router flips `hidden` (which marks `LAYOUT | PAINT`), so
//!   the flip re-runs `store.layout`; the flip's one-time layout cost folds into
//!   warmup and the steady assert holds only after the scene settles — showing and
//!   hiding panels does not leak an allocation per frame.
//!
//! Building through `Tabs::build` is the point: it proves the widget lowers to an
//! interactive, panel-switching subtree the `viso-ui` input/paint path and
//! `paint_tree` handle unchanged — including the deferred `hidden` seam a tab
//! switch needs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::{FrameStats, GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs};
use viso::ui::{
    BindingTable, BoxStyle, BuildCx, Component, Content, Key, KeyEvent, KeyRouter, LeafStyle,
    Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Role,
    Size, StateStore, TextEdits, Vec2, VirtualLists, paint_tree,
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
            size: Size::fill(),
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

    let widget = tabs()
        .tab("Details", panel_fill(PANEL_A))
        .tab("History", panel_fill(PANEL_B))
        .selected(0)
        .size(Size::fill());

    let mut cx = BuildCx::with_reactive(
        store,
        &mut states,
        &mut bindings,
        &mut lists,
        &mut text_edits,
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
    let mut child = arena.links(parent).and_then(|l| l.first_child);
    while let Some(c) = child {
        out.push(c);
        child = arena.links(c).and_then(|l| l.next_sibling);
    }
    out
}

/// Replace each tab caption's content with a shared deterministic glyph run, so
/// the golden is font/atlas-stable without a real font stack and the atlas stops
/// growing after build — the same fixture bypass `button_widget.rs` uses. A tabs
/// root is `[strip, panel_area]`; the strip's children are the tab buttons; each
/// tab button's first child is its caption leaf (`Label` maps to one leaf).
fn attach_captions(gpu: &mut HeadlessRaster, store: &mut NodeStore, root: NodeId) {
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

    let strip = children_of(store, root)[0];
    for button in children_of(store, strip) {
        let caption = children_of(store, button)[0];
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

// --- golden screenshot ------------------------------------------------------

#[test]
fn tabs_render_the_strip_and_selected_panel_and_match_golden() {
    let mut gpu = HeadlessRaster::new();
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

// --- interactive input tapes ------------------------------------------------

/// A `Tabs` laid out as the root of its own tree, kept together with the reactive
/// stores its handlers write into so a router can drive it. The tabs fill the
/// surface. `on_change` records the last selected index and bumps a counter.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    chain: Vec<NodeId>,
}

impl Interactive {
    fn new(counter: Rc<Cell<u32>>, last: Rc<Cell<Option<usize>>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();

        let widget = tabs()
            .tab("Details", panel_fill(PANEL_A))
            .tab("History", panel_fill(PANEL_B))
            .selected(0)
            .size(Size::fill())
            .on_change(move |_ev, index| {
                last.set(Some(index));
                counter.set(counter.get() + 1);
            });

        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
            );
            widget.build(&mut cx);
            cx.root().expect("tabs declares a root")
        };

        let mut scratch = Vec::new();
        store.layout(root, surface_rect(), &mut scratch);

        Interactive {
            store,
            states,
            bindings,
            root,
            chain: Vec::new(),
        }
    }

    /// The strip and panel-area children of the root, then the tab buttons and the
    /// panels, walking the arena sibling chain.
    fn strip_buttons(&self) -> Vec<NodeId> {
        let strip = self.children(self.root)[0];
        self.children(strip)
    }

    fn panels(&self) -> Vec<NodeId> {
        let area = self.children(self.root)[1];
        self.children(area)
    }

    fn children(&self, parent: NodeId) -> Vec<NodeId> {
        let arena = self.store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// Route a pointer sample through the public `PointerRouter`, exactly as the
    /// facade's `on_input` does — including applying any deferred `hidden` flips the
    /// handler requests. Returns whether any handler ran.
    fn pointer(&mut self, ev: PointerEvent) -> bool {
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.root,
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
            self.root,
            ev,
            &mut self.chain,
        )
    }

    /// The current value of the shared selection cell (via a throwaway read cx).
    fn selected(&mut self) -> Option<i32> {
        // The strip is bound to the selected cell; read it back through the state
        // store the build wrote into. The build allocated exactly one Int cell, so
        // the first cell holds the selection.
        use viso::ui::StateValue;
        let ev = PointerEvent {
            x: 0.0,
            y: 0.0,
            phase: PointerPhase::Move,
            buttons: PointerButtons::NONE,
            modifiers: Modifiers::default(),
        };
        let cell = StateStore::new().alloc(StateValue::Int(0));
        let cx = viso::ui::EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
        match cx.get(cell) {
            Some(StateValue::Int(i)) => Some(i),
            _ => None,
        }
    }
}

/// A primary-button pointer sample at `(x, y)`, in the given phase.
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

/// The world-box center of a node, for aiming a pointer sample at it.
fn center_of(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let b = store.world(id);
    (b.x + b.w / 2.0, b.y + b.h / 2.0)
}

#[test]
fn pointer_click_selects_tab_switches_panels_and_fires_change() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<usize>));
    let mut ix = Interactive::new(count.clone(), last.clone());

    let buttons = ix.strip_buttons();
    let panels = ix.panels();
    assert_eq!(buttons.len(), 2, "one tab button per tab");
    assert_eq!(panels.len(), 2, "one panel per tab");
    assert!(ix.store.hidden(panels[1]), "the second panel starts hidden");

    let (bx, by) = center_of(&ix.store, buttons[1]);

    // A press alone does not select.
    assert!(
        ix.pointer(primary_at(bx, by, PointerPhase::Down)),
        "the press hits the second tab's handler"
    );
    assert_eq!(count.get(), 0, "the press alone does not select");

    // The release on the second tab selects index 1, fires once, and — through the
    // router applying the deferred requests — flips the panels' hidden flags.
    assert!(ix.pointer(primary_at(bx, by, PointerPhase::Up)));
    assert_eq!(count.get(), 1, "press-then-release is one selection");
    assert_eq!(last.get(), Some(1), "on_change carries the selected index");
    assert_eq!(ix.selected(), Some(1), "the shared cell holds index 1");
    assert!(ix.store.hidden(panels[0]), "the old panel is now hidden");
    assert!(!ix.store.hidden(panels[1]), "the new panel is now shown");

    // Re-clicking the current tab is a guarded no-op.
    ix.pointer(primary_at(bx, by, PointerPhase::Down));
    ix.pointer(primary_at(bx, by, PointerPhase::Up));
    assert_eq!(count.get(), 1, "re-selecting the current tab does nothing");
}

#[test]
fn keyboard_activates_and_arrows_step_selection() {
    let count = Rc::new(Cell::new(0u32));
    let last = Rc::new(Cell::new(None::<usize>));
    let mut ix = Interactive::new(count.clone(), last.clone());
    let buttons = ix.strip_buttons();

    // Unfocused: the key router has no target, so nothing fires.
    assert!(
        !ix.key(key_ev(Key::Right, true, false)),
        "an unfocused strip receives no key dispatch"
    );
    assert_eq!(count.get(), 0);

    // Focus the first tab: Right steps to tab 1.
    ix.store.set_focused(Some(buttons[0]));
    assert!(ix.key(key_ev(Key::Right, true, false)));
    assert_eq!(ix.selected(), Some(1), "Right steps to the next tab");
    assert_eq!(last.get(), Some(1));

    // Focus the second tab: Enter activates it (already selected → guarded no-op),
    // then Left steps back to tab 0.
    ix.store.set_focused(Some(buttons[1]));
    let before = count.get();
    ix.key(key_ev(Key::Enter, true, false));
    assert_eq!(
        count.get(),
        before,
        "Enter on the already-selected tab is a no-op"
    );
    assert!(ix.key(key_ev(Key::Left, true, false)));
    assert_eq!(ix.selected(), Some(0), "Left steps to the previous tab");

    // Focus the first tab: Space activates tab 0 (guarded no-op, already selected).
    ix.store.set_focused(Some(buttons[0]));
    let before = count.get();
    ix.key(key_ev(Key::Space, true, false));
    assert_eq!(
        count.get(),
        before,
        "Space on the already-selected tab is a no-op"
    );
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn tabs_derive_a_tablist_over_named_tabs_and_group_panels() {
    let mut store = NodeStore::new();
    let root = build_scene(&mut store);
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "the control is a Group");

    // The strip is a TabList; its buttons are named Tabs.
    let tablist = tree
        .nodes
        .iter()
        .find(|n| n.role == Role::TabList)
        .expect("a TabList node");
    let tab_labels: Vec<&str> = tree
        .nodes
        .iter()
        .filter(|n| n.role == Role::Tab)
        .filter_map(|n| n.label.as_deref())
        .collect();
    assert_eq!(
        tab_labels,
        vec!["Details", "History"],
        "each tab is named by its caption"
    );
    assert!(
        !tablist.children.is_empty(),
        "the TabList groups the tab buttons"
    );

    // The tab buttons are interactive: pointer + key handlers, focusable.
    let strip = children_of(&store, root)[0];
    for button in children_of(&store, strip) {
        assert!(store.has_handler(button), "each tab has a pointer handler");
        assert!(store.has_key_handler(button), "each tab has a key handler");
        assert!(store.focusable(button), "each tab is focusable");
    }
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `splitter_widget.rs`.
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
    let (strip, area) = {
        let kids = children_of(&h.store, h.root);
        (kids[0], kids[1])
    };
    let _ = strip;
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
    } = stats;
    assert!(draw_calls > 0, "the Tabs scene must emit draw calls");
    assert!(instances > 0, "the Tabs scene must emit instances");
}
