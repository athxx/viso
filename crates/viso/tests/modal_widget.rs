//! Section 71 validation pack for the `Modal` control — a dialog layered over the
//! whole scene that dims the background, takes focus, and traps keyboard navigation
//! inside its content while open — driven through the public facade. Mirrors
//! `popup_widget.rs` (golden + input tapes + a11y + allocation) with the modal's own
//! structure: a scrim and the dialog content build once into an overlapping stretched
//! column, both flagged overlays (top layer) starting hidden, the scrim authored
//! before the content so it paints under it. Opening flips both `hidden` flags, moves
//! focus into the content, and installs a *focus scope* on the content so Tab cycles
//! only inside the dialog; closing releases the scope, restores focus to the pre-open
//! node, and fires `on_dismiss`. All store effects (`hidden`, focus, focus-scope) ride
//! the deferred-request seam the router applies after a handler returns.
//!
//! - **golden screenshot** — build a `Modal` with a full-surface scrim of one color
//!   and a full-surface content of a second, opaque color, opened, lay it out over a
//!   fixed surface, and confirm the pixels match a blessed baseline. Both are
//!   overlays; the scrim is authored first, so the top layer paints scrim then
//!   content: an opened modal shows the content color over the scrim over the whole
//!   region, proving the overlay order (content on top of scrim). Pure quads — no font
//!   fixture;
//! - **keyboard input tape** — with the content focused, `KeyRouter::route_key` closes
//!   the open modal on Escape, flipping the content's `hidden` flag, moving the `open`
//!   cell, clearing the focus scope, restoring focus, and firing `on_dismiss`; an
//!   unfocused modal receives no key dispatch, and Escape on an already-closed modal
//!   is a no-op;
//! - **programmatic input tape** — a captured `ModalHandle`, driven inside an
//!   `EventCx` as the router would run it (draining the deferred `hidden`, focus, and
//!   focus-scope requests and applying them), opens/closes/toggles: the flips it defers
//!   are applied, the `open` cell moves, and `on_dismiss` fires once on each close; a
//!   repeated open and a repeated close are de-duped no-ops;
//! - **focus-trap input tape** — with a focusable sibling *outside* the modal's content
//!   scope, opening the modal installs a focus scope on the content, and `focus_next`
//!   (Tab) then cycles only among the content's own focusables — it never lands on the
//!   outside sibling. Closing releases the scope (Tab reaches the whole tree again) and
//!   restores focus to the node that held it before the modal opened;
//! - **a11y snapshot** — the derived tree carries a `Group` root over a `Dialog`
//!   content; the content is focusable and declares a key handler for Escape;
//! - **allocation profile** — a warmed-up open `Modal` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), exercised across an
//!   open→close→open flip: each `hidden` flip marks `LAYOUT | PAINT`, so it re-runs
//!   `store.layout`; the flips' one-time layout cost folds into warmup and the steady
//!   assert holds only after the scene settles on the open dialog (the state that
//!   actually renders — a modal paints nothing while closed) — showing and hiding the
//!   dialog does not leak an allocation per frame.
//!
//! Building through `Modal::build` is the point: it proves the widget lowers to a
//! scrim-plus-content overlay subtree the `viso-ui` input/paint path and `paint_tree`
//! handle unchanged — including the top-layer overlay order, the deferred `hidden`
//! seam, and the focus-scope trap an open/close needs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    BindingTable, BoxStyle, BuildCx, Component, EventCx, Key, KeyEvent, KeyRouter, LeafStyle,
    Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, Role,
    SemanticProjector, Size, StateId, StateStore, StateValue, TextEdits, VirtualLists, focus_next,
    paint_tree,
};
use viso::widgets::{ModalHandle, ModalHandleSlot, modal};

const W: u32 = 240;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// The scrim's fill — the dimming backdrop under the content. Opaque here (a=1) so the
/// golden is deterministic regardless of blend order; a real scrim is translucent.
const SCRIM: Rgba = Rgba {
    r: 0.10,
    g: 0.10,
    b: 0.12,
    a: 1.0,
};
/// The content's fill — the dialog panel over the scrim. In the opened golden it paints
/// over the scrim's whole region, so the golden shows this color, proving the overlay
/// order (content on top of scrim).
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
/// scrim's region and, when open, the content's region on top.
fn fill(color: Rgba) -> impl Fn(&mut BuildCx<'_>) + 'static {
    move |cx| {
        cx.leaf(LeafStyle {
            size: Size::fill(),
            style: BoxStyle::solid(color),
        });
    }
}

/// Build a `Modal` with a colored scrim and colored content filling the surface, then
/// open it (so the scrim and content overlays are shown for the golden), and return the
/// root. Modal authors a reactive cell, so it builds through a reactive cx; opening for
/// the golden flips both `hidden` flags directly (the same effect the router applies for
/// a `ModalHandle::open`) — build leaves both hidden/closed.
fn build_open_scene(store: &mut NodeStore) -> NodeId {
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let widget = modal()
        .scrim(SCRIM)
        .content(fill(CONTENT))
        .size(Size::fill());

    let root = {
        let mut cx = BuildCx::with_reactive(
            store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        widget.build(&mut cx);
        cx.root().expect("modal declares a root")
    };
    // Open the modal: show both overlays (build leaves scrim and content hidden). With
    // the scrim on, the root's children are [scrim, content] in author order.
    let parts = children_of(store, root);
    store.set_hidden(parts[0], false);
    store.set_hidden(parts[1], false);
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
fn open_modal_paints_the_content_over_the_scrim_and_matches_golden() {
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/modal_widget.bgra8")
}

// --- interactive input tapes ------------------------------------------------

/// A `Modal` laid out as the root of its own tree, kept together with the reactive
/// stores its handlers write into so a router (and a captured `ModalHandle`) can drive
/// it. The modal fills the surface. `on_dismiss` bumps a counter.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    chain: Vec<NodeId>,
    handle: ModalHandle,
    /// The shared `open` cell the build authored — Modal authors exactly one `Bool`
    /// state cell, so a fresh `StateStore` allocating one `Bool` yields the same handle
    /// the build produced (there is no public bare-`StateId` ctor).
    open: StateId,
}

impl Interactive {
    fn new(counter: Rc<Cell<u32>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();

        let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
        let widget = modal()
            .scrim(SCRIM)
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
                &mut projectors,
            );
            widget.build(&mut cx);
            cx.root().expect("modal declares a root")
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

    /// The scrim and content nodes (the root's direct children), in author order.
    fn parts(&self) -> Vec<NodeId> {
        children_of(&self.store, self.root)
    }

    /// The dimming scrim node.
    fn scrim(&self) -> NodeId {
        self.parts()[0]
    }

    /// The dialog content node.
    fn content(&self) -> NodeId {
        self.parts()[1]
    }

    /// Route a key sample through the public `KeyRouter` to the focused node, applying
    /// the deferred `hidden`/focus/focus-scope requests exactly as the facade's
    /// `on_input` does (`key_dispatch` drains and applies them internally).
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

    /// Drive a `ModalHandle` action (open/close/toggle) as a router would: run it inside
    /// a throwaway `EventCx` (lending the current focus in, as `key_dispatch` does), take
    /// the deferred `hidden`/focus/focus-scope requests, and apply them to the store.
    fn drive(&mut self, act: impl FnOnce(&ModalHandle, &mut EventCx<'_>)) {
        let ev = read_pointer();
        let handle = self.handle.clone();
        let (hidden, focus, scope) = {
            let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            cx.__set_focused(self.store.focused());
            act(&handle, &mut cx);
            (
                cx.__take_hidden_requests(),
                cx.__take_focus_request(),
                cx.__take_focus_scope_request(),
            )
        };
        for (id, h) in hidden {
            self.store.set_hidden(id, h);
        }
        if let Some(target) = focus {
            self.store.set_focused(target);
        }
        if let Some(scope_req) = scope {
            self.store.set_focus_scope(scope_req);
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
fn keyboard_escape_closes_the_open_modal_restores_focus_and_fires_dismiss() {
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
        "an unfocused modal receives no key dispatch"
    );
    assert_eq!(count.get(), 0);

    // A pre-open focus target that close should restore to. Open programmatically: the
    // handle snapshots this into `restore_focus`, shows the content, installs the focus
    // scope on it, and moves focus into it.
    ix.store.set_focused(Some(ix.root));
    ix.drive(|m, ev| m.open(ev));
    assert_eq!(ix.is_open(), Some(true), "the modal is open");
    assert!(!ix.store.hidden(content), "the content is shown");
    assert_eq!(
        ix.store.focused(),
        Some(content),
        "opening moves focus into the dialog content"
    );
    assert_eq!(
        ix.store.focus_scope(),
        Some(content),
        "opening installs a focus scope on the content (Tab is trapped)"
    );

    // Escape closes: hides the content, moves the open cell, releases the scope,
    // restores focus to the pre-open node, and fires on_dismiss once.
    assert!(ix.key(key_ev(Key::Escape, true, false)));
    assert_eq!(ix.is_open(), Some(false), "Escape closes the modal");
    assert!(ix.store.hidden(content), "the content is now hidden");
    assert_eq!(
        ix.store.focus_scope(),
        None,
        "closing releases the focus scope"
    );
    assert_eq!(
        ix.store.focused(),
        Some(ix.root),
        "closing restores focus to the pre-open node"
    );
    assert_eq!(count.get(), 1, "the Escape dismiss fires on_dismiss once");

    // Escape on an already-closed modal is a no-op (the content is unfocused now that
    // focus was restored, and the guard would short-circuit regardless).
    let before = count.get();
    ix.key(key_ev(Key::Escape, true, false));
    assert_eq!(ix.is_open(), Some(false), "the modal stays closed");
    assert_eq!(count.get(), before, "and does not fire again");
}

#[test]
fn handle_open_close_toggle_flip_the_content_and_dedupe() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(count.clone());
    let content = ix.content();
    let scrim = ix.scrim();

    // Open reveals the content, moves the cell, does not fire on_dismiss. (The scrim is
    // flipped by the app/router alongside; the handle owns the content flip.)
    ix.drive(|m, ev| m.open(ev));
    assert_eq!(ix.is_open(), Some(true), "open moves the open cell");
    assert!(!ix.store.hidden(content), "the content is shown");
    assert!(ix.store.hidden(scrim), "the handle flips only the content");
    assert_eq!(count.get(), 0, "opening does not fire on_dismiss");

    // A repeated open is a de-duped no-op.
    let before = count.get();
    ix.drive(|m, ev| m.open(ev));
    assert_eq!(ix.is_open(), Some(true), "a repeated open is a no-op");
    assert_eq!(count.get(), before, "and does not fire");

    // Close hides the content, moves the cell, fires on_dismiss once.
    ix.drive(|m, ev| m.close(ev));
    assert_eq!(ix.is_open(), Some(false), "close moves the open cell");
    assert!(ix.store.hidden(content), "the content is hidden");
    assert_eq!(count.get(), 1, "close fires on_dismiss once");

    // A repeated close is a de-duped no-op.
    let before = count.get();
    ix.drive(|m, ev| m.close(ev));
    assert_eq!(ix.is_open(), Some(false), "a repeated close is a no-op");
    assert_eq!(count.get(), before, "and does not fire again");

    // Toggle flips open, then closed (the close fires on_dismiss).
    ix.drive(|m, ev| m.toggle(ev));
    assert_eq!(ix.is_open(), Some(true), "toggle opens a closed modal");
    assert!(!ix.store.hidden(content), "the content is shown");
    let before = count.get();
    ix.drive(|m, ev| m.toggle(ev));
    assert_eq!(ix.is_open(), Some(false), "toggle closes an open modal");
    assert!(ix.store.hidden(content), "the content is hidden");
    assert_eq!(count.get(), before + 1, "the toggle-close fires on_dismiss");
}

#[test]
fn opening_traps_tab_inside_the_content_and_closing_releases_and_restores_focus() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(count.clone());
    let content = ix.content();

    // Author a focusable sibling *outside* the modal content scope: a leaf appended
    // under the root (via a `with_parent` build), focusable, so a whole-tree Tab ring
    // would include it. It is not a modal descendant, so an installed content scope must
    // exclude it.
    let root = ix.root;
    let outside = {
        let mut cx = BuildCx::with_parent(&mut ix.store, &mut ix.states, &mut ix.bindings, root);
        let leaf = cx.leaf(LeafStyle {
            size: Size::fixed(10.0, 10.0),
            style: BoxStyle::NONE,
        });
        cx.focusable(leaf, true);
        leaf.id()
    };
    {
        let mut scratch = Vec::new();
        ix.store.layout(ix.root, surface_rect(), &mut scratch);
    }

    // With no scope installed (closed modal), Tab reaches the whole tree, including the
    // outside sibling. Start focus on the outside node and confirm the ring can be there.
    ix.store.set_focused(Some(outside));
    assert_eq!(
        ix.store.focus_scope(),
        None,
        "no scope is installed while closed"
    );

    // Open the modal: this installs a focus scope on the content and moves focus into it.
    ix.drive(|m, ev| m.open(ev));
    assert_eq!(
        ix.store.focus_scope(),
        Some(content),
        "opening installs the content focus scope"
    );
    assert_eq!(
        ix.store.focused(),
        Some(content),
        "opening moves focus into the content"
    );

    // Tab (and Shift-Tab) now cycle only within the content subtree — the outside
    // sibling is never reached. The content itself is the sole focusable in the scope
    // (its fill child is not focusable), so the ring stays on it.
    for forward in [true, false, true] {
        let landed = focus_next(&mut ix.store, ix.root, forward);
        assert_eq!(
            landed,
            Some(content),
            "Tab stays inside the content scope (forward={forward})"
        );
        assert_ne!(
            landed,
            Some(outside),
            "Tab never escapes the scope to the outside sibling"
        );
    }

    // Close releases the scope and restores focus to the pre-open node (the outside
    // sibling that held focus when we opened).
    ix.drive(|m, ev| m.close(ev));
    assert_eq!(
        ix.store.focus_scope(),
        None,
        "closing releases the focus scope"
    );
    assert_eq!(
        ix.store.focused(),
        Some(outside),
        "closing restores focus to the pre-open node outside the modal"
    );

    // With the scope released, Tab reaches the whole tree again: the outside sibling is
    // once more in the ring (content is hidden but still focusable, so the ring has both;
    // the point is the outside node is reachable again).
    let ring_after = focus_next(&mut ix.store, ix.root, true);
    assert!(
        ring_after == Some(outside) || ring_after == Some(content),
        "with no scope, Tab traverses the whole tree again (landed on {ring_after:?})"
    );
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn modal_derives_a_group_root_over_a_dialog_content() {
    let mut store = NodeStore::new();
    // The a11y tree does not depend on open/closed; build the opened scene so the
    // content is present and derivable.
    let root = build_open_scene(&mut store);
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "the modal is a Group container");

    // The content node is a Dialog; the scrim is a plain fill leaf with no semantics,
    // so the Dialog child under the root is the content.
    let child_roles: Vec<Role> = node.children.iter().map(|&i| tree.nodes[i].role).collect();
    assert!(
        child_roles.contains(&Role::Dialog),
        "the content is a Dialog under the modal root"
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
/// allocations are never counted. Mirrors `popup_widget.rs`.
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
fn steady_modal_frame_is_allocation_free_including_after_an_open_close_flip() {
    let mut h = setup_alloc();

    // The scene starts open (build_open_scene shows the scrim and content). Exercise a
    // full open→close→open flip through direct `set_hidden` on both overlays (the same
    // effect the router applies), settling with layout after each flip, and measure the
    // steady *open* frame — the state that actually renders the dialog (a modal with no
    // anchor paints nothing while closed, so the meaningful steady frame is the open one).
    // Each flip marks LAYOUT | PAINT; the one-time flip/layout cost folds into warmup.
    let parts = children_of(&h.store, h.root);
    let mut scratch = Vec::new();
    // Close (hide both), settle.
    h.store.set_hidden(parts[0], true);
    h.store.set_hidden(parts[1], true);
    h.store.layout(h.root, surface_rect(), &mut scratch);
    // Re-open (show both), settle: the measured steady scene is the open dialog.
    h.store.set_hidden(parts[0], false);
    h.store.set_hidden(parts[1], false);
    h.store.layout(h.root, surface_rect(), &mut scratch);

    // Warm up until the frame path reaches steady state on the post-flip scene.
    for _ in 0..4 {
        frame(&mut h);
    }
    // Grow the reused paint buffer to its steady capacity so a later `paint_tree` into it
    // does not reallocate.
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
            "frame {i}: frame_stats changed for an unchanged Modal scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged Modal scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged Modal scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged Modal scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a Modal frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the Modal scene must emit draw calls");
    assert!(instances > 0, "the Modal scene must emit instances");
}
