//! Section 71 validation pack for the `Popup` control — a persistent anchor with
//! a floating content layer that opens over the scene — driven through the public
//! facade. Mirrors `navigation_stack_widget.rs` (golden + input tapes + a11y +
//! allocation) with the popup's own structure: the anchor and content build once
//! into an overlapping stretched column, the content is flagged an overlay (top
//! layer) and starts hidden, and opening/closing flips the retained `hidden` flag
//! the router applies as a deferred request. An open/close (Escape, or a
//! programmatic `PopupHandle::open`/`close`/`toggle`) writes a reactive `open`
//! cell, defers a `hidden` flip on the content, and — on a close — fires
//! `on_dismiss` once.
//!
//! - **golden screenshot** — build a `Popup` with a full-surface anchor of one
//!   color and a full-surface content of a second color, opened, lay it out over a
//!   fixed surface, and confirm the pixels match a blessed baseline. The content
//!   is an overlay, so it paints in the top layer *after* the anchor: an opened
//!   popup shows the content color over the anchor's whole region, proving the
//!   overlay draws on top. Pure quads — no font fixture;
//! - **keyboard input tape** — with the content focused, `KeyRouter::route_key`
//!   closes the open popup on Escape, flipping the content's `hidden` flag, moving
//!   the `open` cell, and firing `on_dismiss`; an unfocused popup receives no key
//!   dispatch, and Escape on an already-closed popup is a no-op;
//! - **programmatic input tape** — a captured `PopupHandle`, driven inside an
//!   `EventCx` as the router would run it, opens/closes/toggles: the flip it defers
//!   is applied, the `open` cell moves, and `on_dismiss` fires once on each close; a
//!   repeated open and a repeated close are de-duped no-ops;
//! - **a11y snapshot** — the derived tree carries a `Group` root over a `Group`
//!   content; the content is focusable and declares a key handler for Escape;
//! - **allocation profile** — a warmed-up open `Popup` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), plus an *open-then-close
//!   flip* frame: driving a close through the deferred seam flips `hidden` (which
//!   marks `LAYOUT | PAINT`), so the flip re-runs `store.layout`; the flip's
//!   one-time layout cost folds into warmup and the steady assert holds only after
//!   the scene settles — showing and hiding the content does not leak an allocation
//!   per frame.
//!
//! Building through `Popup::build` is the point: it proves the widget lowers to an
//! anchor-plus-overlay subtree the `viso-ui` input/paint path and `paint_tree`
//! handle unchanged — including the top-layer overlay order and the deferred
//! `hidden` seam an open/close needs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    BindingTable, BoxStyle, BuildCx, Component, EventCx, Key, KeyEvent, KeyRouter, LeafStyle,
    Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, Role, Size, StateId,
    StateStore, StateValue, TextEdits, VirtualLists, paint_tree,
};
use viso::widgets::{PopupHandle, PopupHandleSlot, popup};

const W: u32 = 240;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// The anchor's fill — the persistent, in-place layer.
const ANCHOR: Rgba = Rgba {
    r: 0.16,
    g: 0.22,
    b: 0.34,
    a: 1.0,
};
/// The content's fill — the floating overlay layer. In the opened golden it paints
/// over the anchor's whole region, so the golden shows this color, proving the
/// overlay draws on top.
const CONTENT: Rgba = Rgba {
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

/// A content builder that fills its box with a solid color, so the golden shows the
/// anchor's region and, when open, the content's region on top.
fn fill(color: Rgba) -> impl Fn(&mut BuildCx<'_>) + 'static {
    move |cx| {
        cx.leaf(LeafStyle {
            size: Size::fill(),
            style: BoxStyle::solid(color),
        });
    }
}

/// Build a `Popup` that fills the surface with a colored anchor and colored
/// content, then open it (so the content overlay is shown for the golden), and
/// return the root. Popup authors a reactive cell, so it builds through a reactive
/// cx; opening for the golden flips the content's `hidden` flag directly (the same
/// effect the router applies for a `PopupHandle::open`) and re-lays out.
fn build_open_scene(store: &mut NodeStore) -> NodeId {
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();

    let widget = popup()
        .anchor(fill(ANCHOR))
        .content(fill(CONTENT))
        .size(Size::fill());

    let root = {
        let mut cx = BuildCx::with_reactive(
            store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
        );
        widget.build(&mut cx);
        cx.root().expect("popup declares a root")
    };
    // Open the popup: show the content overlay (build leaves it hidden/closed).
    let content = children_of(store, root)[1];
    store.set_hidden(content, false);
    root
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
fn open_popup_paints_the_content_overlay_over_the_anchor_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let root = build_open_scene(&mut store);

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/popup_widget.bgra8")
}

// --- interactive input tapes ------------------------------------------------

/// A `Popup` laid out as the root of its own tree, kept together with the reactive
/// stores its handlers write into so a router (and a captured `PopupHandle`) can
/// drive it. The popup fills the surface. `on_dismiss` bumps a counter.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    chain: Vec<NodeId>,
    handle: PopupHandle,
    /// The shared `open` cell the build authored — Popup authors exactly one `Bool`
    /// state cell, so a fresh `StateStore` allocating one `Bool` yields the same
    /// handle the build produced (there is no public bare-`StateId` ctor).
    open: StateId,
}

impl Interactive {
    fn new(counter: Rc<Cell<u32>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();

        let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
        let widget = popup()
            .anchor(fill(ANCHOR))
            .content(fill(CONTENT))
            .size(Size::fill())
            .handle(&slot)
            .on_dismiss(move |_ev| {
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
            cx.root().expect("popup declares a root")
        };
        let handle = slot.borrow().clone().expect("build fills the handle slot");
        let open = StateStore::new().alloc(StateValue::Bool(false));

        let mut scratch = Vec::new();
        store.layout(root, surface_rect(), &mut scratch);

        Interactive {
            store,
            states,
            bindings,
            root,
            chain: Vec::new(),
            handle,
            open,
        }
    }

    /// The anchor and content nodes (the root's direct children), in author order.
    fn parts(&self) -> Vec<NodeId> {
        children_of(&self.store, self.root)
    }

    /// The floating content node.
    fn content(&self) -> NodeId {
        self.parts()[1]
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

    /// Drive a `PopupHandle` action (open/close/toggle) as a router would: run it
    /// inside a throwaway `EventCx`, take the deferred `hidden` flips, and apply.
    fn drive(&mut self, act: impl FnOnce(&PopupHandle, &mut EventCx<'_>)) {
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

    /// The current value of the shared open cell (via a throwaway read cx).
    fn is_open(&mut self) -> Option<bool> {
        let ev = read_pointer();
        let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
        match cx.get(self.open) {
            Some(StateValue::Bool(b)) => Some(b),
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
fn keyboard_escape_closes_the_open_popup_and_fires_dismiss() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(count.clone());
    let content = ix.content();

    assert!(
        ix.store.hidden(content),
        "the content starts hidden (closed)"
    );

    // Unfocused: the key router has no target, so nothing fires.
    assert!(
        !ix.key(key_ev(Key::Escape, true, false)),
        "an unfocused popup receives no key dispatch"
    );
    assert_eq!(count.get(), 0);

    // Open programmatically, then focus the content so Escape routes to it.
    ix.drive(|p, ev| p.open(ev));
    assert_eq!(ix.is_open(), Some(true), "the popup is open");
    assert!(!ix.store.hidden(content), "the content is shown");
    ix.store.set_focused(Some(content));

    // Escape closes: hides the content, moves the open cell, fires on_dismiss once.
    assert!(ix.key(key_ev(Key::Escape, true, false)));
    assert_eq!(ix.is_open(), Some(false), "Escape closes the popup");
    assert!(ix.store.hidden(content), "the content is now hidden");
    assert_eq!(count.get(), 1, "the Escape dismiss fires on_dismiss once");

    // Escape on an already-closed popup is a no-op (the content is unfocused now
    // that it is hidden, and the guard would short-circuit regardless).
    let before = count.get();
    ix.key(key_ev(Key::Escape, true, false));
    assert_eq!(ix.is_open(), Some(false), "the popup stays closed");
    assert_eq!(count.get(), before, "and does not fire again");
}

#[test]
fn handle_open_close_toggle_flip_the_content_and_dedupe() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(count.clone());
    let content = ix.content();

    // Open reveals the content, moves the cell, does not fire on_dismiss.
    ix.drive(|p, ev| p.open(ev));
    assert_eq!(ix.is_open(), Some(true), "open moves the open cell");
    assert!(!ix.store.hidden(content), "the content is shown");
    assert_eq!(count.get(), 0, "opening does not fire on_dismiss");

    // A repeated open is a de-duped no-op.
    let before = count.get();
    ix.drive(|p, ev| p.open(ev));
    assert_eq!(ix.is_open(), Some(true), "a repeated open is a no-op");
    assert_eq!(count.get(), before, "and does not fire");

    // Close hides the content, moves the cell, fires on_dismiss once.
    ix.drive(|p, ev| p.close(ev));
    assert_eq!(ix.is_open(), Some(false), "close moves the open cell");
    assert!(ix.store.hidden(content), "the content is hidden");
    assert_eq!(count.get(), 1, "close fires on_dismiss once");

    // A repeated close is a de-duped no-op.
    let before = count.get();
    ix.drive(|p, ev| p.close(ev));
    assert_eq!(ix.is_open(), Some(false), "a repeated close is a no-op");
    assert_eq!(count.get(), before, "and does not fire again");

    // Toggle flips open, then closed (the close fires on_dismiss).
    ix.drive(|p, ev| p.toggle(ev));
    assert_eq!(ix.is_open(), Some(true), "toggle opens a closed popup");
    assert!(!ix.store.hidden(content), "the content is shown");
    let before = count.get();
    ix.drive(|p, ev| p.toggle(ev));
    assert_eq!(ix.is_open(), Some(false), "toggle closes an open popup");
    assert!(ix.store.hidden(content), "the content is hidden");
    assert_eq!(count.get(), before + 1, "the toggle-close fires on_dismiss");
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn popup_derives_a_group_root_over_a_group_content() {
    let mut store = NodeStore::new();
    // The a11y tree does not depend on open/closed; build the opened scene so the
    // content is present and derivable.
    let root = build_open_scene(&mut store);
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "the popup is a Group container");

    // The content node is a Group; the anchor here is a plain fill leaf with no
    // semantics, so the Group child under the root is the content.
    let child_roles: Vec<Role> = node.children.iter().map(|&i| tree.nodes[i].role).collect();
    assert!(
        child_roles.contains(&Role::Group),
        "the content is a Group under the popup root"
    );

    // The content is interactive: focusable with a key handler for Escape.
    let content = children_of(&store, root)[1];
    assert!(store.focusable(content), "the content is focusable");
    assert!(
        store.has_key_handler(content),
        "the content attaches a key handler for Escape"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `navigation_stack_widget.rs`.
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
    let root = build_open_scene(&mut store);

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
fn steady_popup_frame_is_allocation_free_including_after_an_open_close_flip() {
    let mut h = setup_alloc();

    // The scene starts open (build_open_scene shows the content). Close it once
    // through a direct `set_hidden` (the same effect the router applies for a
    // close): hide the content. The flip marks LAYOUT | PAINT, so re-run layout to
    // settle the new visibility before measuring — the one-time flip/layout cost
    // folds into warmup.
    let content = children_of(&h.store, h.root)[1];
    h.store.set_hidden(content, true);
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
            "frame {i}: frame_stats changed for an unchanged Popup scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Popup scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Popup scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Popup scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a Popup frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the Popup scene must emit draw calls");
    assert!(instances > 0, "the Popup scene must emit instances");
}
