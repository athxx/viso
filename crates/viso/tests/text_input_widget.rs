//! Section 71 validation pack for the `TextInput` control — a single-line
//! editable text field with selection and IME composition — driven through the
//! public facade. TextInput is the first *editable* control, so beyond the
//! Toggle/Button pack (golden + a11y + allocation + a pointer and a keyboard
//! input tape) it adds an IME tape and drives real edits end to end through the
//! same router + reconcile seam the facade's `AppDriver` uses.
//!
//! Unlike Toggle (a track flex + thumb + caption), a `TextInput` lowers to a
//! *single* editable leaf: the root IS the text-bearing node. So the golden
//! attaches a deterministic glyph run to the root leaf directly, and the input
//! tapes read the applied buffer back off that same node.
//!
//! - **golden screenshot + measure** — build a `TextInput` with `TextInput::build`
//!   inside a padded dark `View`, attach a deterministic glyph run to the field
//!   leaf (the facade `TextShaper` is `pub(crate)`, so integration tests shape via
//!   the shared `viso-render` fixtures rather than a real font stack), lay it out,
//!   and confirm the pixels match a blessed baseline;
//! - **pointer input tape** — `PointerRouter::route`: a primary press over the
//!   field requests focus (click-to-focus); a non-primary button does not;
//! - **keyboard input tape** — with the field focused and its buffer registered,
//!   `KeyRouter::route_key` + `text_edit::reconcile` apply arrows/Home/End/
//!   Backspace/Delete to the buffer, and the applied `text`/`sel` are read back;
//! - **IME input tape** — `KeyRouter::route_ime` + `reconcile` drive a Preedit
//!   composition then a Commit, and the committed text lands in the buffer;
//! - **a11y snapshot** — a `TextInput`'s derived semantics node is
//!   `Role::TextField` and its accessible name is the authored label;
//! - **allocation profile** — a warmed-up `TextInput` frame allocates nothing per
//!   frame (architecture section 47 hot-path contract), same CountingAlloc +
//!   `frame_stats`/`*_count()` steady-state asserts as `toggle_widget.rs`.
//!
//! The editing tapes are the point: they prove the widget's `on_key` handler,
//! the router's `queue_edits`, and `text_edit::reconcile` compose into a real
//! edit loop the `viso-ui` input path handles unchanged — a stronger claim than
//! the widget-side unit tests, which only assert the *recorded* intents.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::{FrameStats, GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs};
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, Component, Content, ImeEvent, Inset, Key, KeyEvent,
    KeyRouter, Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Role, SemanticProjector, Size, StateStore, TextEdits, Vec2, VirtualLists,
    paint_tree, text_edit,
};
use viso::widgets::{ViewStyle, text_input, view};

const W: u32 = 200;
const H: u32 = 64;
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

/// The intrinsic extent of the glyph run, so the field leaf sizes to it.
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

/// Build a padded dark `View` wrapping a single `TextInput`, and return the root
/// plus the field node. The text input is the container's only child, so its
/// node is the root's first child — and because a `TextInput` lowers to a single
/// editable leaf, that child node IS the text-bearing leaf (no further walk).
fn build_scene(store: &mut NodeStore) -> (NodeId, NodeId) {
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
        text_input("Name").value("Ann").build(cx);
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
    let field_id = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .expect("the view has the text input as its only child");
    (root, field_id)
}

/// Attach the shared deterministic glyph run to the field leaf so the golden is
/// font/atlas-stable without a real font stack — the same fixture bypass the
/// other widget goldens use.
fn attach_glyphs(gpu: &mut HeadlessRaster, store: &mut NodeStore, field_id: NodeId) {
    let tg = test_glyphs([0.0, 0.0], 26.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "text-input-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);

    let natural = glyph_run_natural(&tg.glyphs);
    store.set_content_payload(
        field_id,
        Content::Text {
            glyphs: tg.glyphs.clone(),
            atlas,
            color_glyphs: Vec::new(),
            color_atlas: None,
            color: WHITE,
            natural,
            baseline: 0.0,
            shaped_at_width: None,
            soft_wrap: false,
        },
    );
}

// --- golden screenshot + measure -------------------------------------------

#[test]
fn text_input_renders_its_text_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let (root, field_id) = build_scene(&mut store);
    attach_glyphs(&mut gpu, &mut store, field_id);

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/text_input_widget.bgra8")
}

// --- interactive input tapes ------------------------------------------------

/// A focusable text field laid out as the root of its own tree, kept together
/// with the reactive stores its handlers read/write so a router can drive it.
/// The field fills the surface (root `Fill`) so any central pointer sample hits
/// it. Crucially, `text_edits` is the SAME registry the widget's `cx.text_input`
/// registered the editable `Buffer` into during build — the routers queue intents
/// onto it and `reconcile` folds them in, exactly as the facade's `AppDriver`
/// does. A throwaway registry would silently drop every edit.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    text_edits: TextEdits,
    field: NodeId,
    chain: Vec<NodeId>,
}

impl Interactive {
    /// Build a fill text field seeded with `seed`, laid out over the surface so a
    /// center-of-surface pointer sample lands on it.
    fn new(seed: &str) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();

        let widget = text_input("Name").value(seed).size(Size::fill());

        let field = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            widget.build(&mut cx);
            cx.root().expect("text input declares a root")
        };

        let mut scratch = Vec::new();
        store.layout(field, surface_rect(), &mut scratch);

        Interactive {
            store,
            states,
            bindings,
            text_edits,
            field,
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
            self.field,
            ev,
            &mut self.chain,
        )
    }

    /// Route a key sample to the focused node, then fold the queued edit intents
    /// into the registered buffer — the facade's route-then-reconcile pairing.
    fn key(&mut self, ev: KeyEvent) -> bool {
        let ran = KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
            self.field,
            ev,
            &mut self.chain,
        );
        text_edit::reconcile(&mut self.store, &mut self.text_edits);
        ran
    }

    /// Route an IME event to the focused node, then reconcile — the IME twin of
    /// `key`.
    fn ime(&mut self, ev: ImeEvent) -> bool {
        let ran = KeyRouter::route_ime(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
            self.field,
            ev,
            &mut self.chain,
        );
        text_edit::reconcile(&mut self.store, &mut self.text_edits);
        ran
    }

    /// The applied buffer text after reconcile.
    fn text(&self) -> &str {
        &self
            .text_edits
            .get(self.field)
            .expect("the field has a registered buffer")
            .text
    }

    /// The applied caret position (selection cursor) after reconcile.
    fn caret(&self) -> usize {
        self.text_edits
            .get(self.field)
            .expect("the field has a registered buffer")
            .sel
            .cursor
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

fn key_ev(key: Key, pressed: bool, shift: bool) -> KeyEvent {
    KeyEvent {
        key,
        pressed,
        repeat: false,
        modifiers: Modifiers {
            shift,
            ..Default::default()
        },
    }
}

#[test]
fn primary_press_requests_focus_on_the_field() {
    let mut ix = Interactive::new("hi");
    assert_eq!(ix.store.focused(), None, "no focus before any input");

    assert!(
        ix.pointer(primary_at_center(PointerPhase::Down)),
        "the press hits the field's pointer handler"
    );
    assert_eq!(
        ix.store.focused(),
        Some(ix.field),
        "a primary press focuses the field (click-to-focus)"
    );
}

#[test]
fn non_primary_press_does_not_focus_the_field() {
    let mut ix = Interactive::new("hi");
    let down = PointerEvent {
        buttons: PointerButtons::NONE,
        ..primary_at_center(PointerPhase::Down)
    };
    ix.pointer(down);
    assert_eq!(
        ix.store.focused(),
        None,
        "a non-primary button does not focus the field"
    );
}

#[test]
fn keyboard_editing_applies_to_the_buffer() {
    let mut ix = Interactive::new("abc");
    // A `with_request` buffer seeds the caret at the end of the text.
    assert_eq!(ix.text(), "abc");
    assert_eq!(ix.caret(), 3, "the seed caret sits at the end");

    // Unfocused: the key router has no target, so nothing is queued or applied.
    assert!(
        !ix.key(key_ev(Key::Backspace, true, false)),
        "an unfocused field receives no key dispatch"
    );
    assert_eq!(ix.text(), "abc", "no edit applied while unfocused");

    ix.store.set_focused(Some(ix.field));

    // Backspace deletes the char before the caret.
    assert!(ix.key(key_ev(Key::Backspace, true, false)));
    assert_eq!(ix.text(), "ab", "Backspace removed the last char");
    assert_eq!(ix.caret(), 2);

    // Home moves the caret to the start; Delete then removes the char after it.
    ix.key(key_ev(Key::Home, true, false));
    assert_eq!(ix.caret(), 0, "Home moved the caret to the start");
    ix.key(key_ev(Key::Delete, true, false));
    assert_eq!(ix.text(), "b", "Delete removed the char after the caret");
    assert_eq!(ix.caret(), 0);

    // End moves back to the end of the (now shorter) text.
    ix.key(key_ev(Key::End, true, false));
    assert_eq!(ix.caret(), 1, "End moved the caret to the end");
}

#[test]
fn ime_composition_commits_into_the_buffer() {
    let mut ix = Interactive::new("");
    ix.store.set_focused(Some(ix.field));

    // A preedit shows in-progress composition without committing final text.
    assert!(ix.ime(ImeEvent::Preedit {
        text: "ni".to_string(),
        caret: 2,
    }));

    // The commit replaces the composition with the final text.
    assert!(ix.ime(ImeEvent::Commit {
        text: "你".to_string(),
    }));
    assert_eq!(ix.text(), "你", "the committed text landed in the buffer");
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn text_input_derives_a_textfield_semantics_node_named_by_its_label() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let root = {
        let widget = text_input("Name");
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        widget.build(&mut cx);
        cx.root().expect("text input declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let tree = store.derive_semantics(root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(
        node.role,
        Role::TextField,
        "a TextInput derives Role::TextField"
    );
    assert_eq!(
        node.label.as_deref(),
        Some("Name"),
        "the accessible name is the authored label"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `toggle_widget.rs`.
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
    let (root, field_id) = build_scene(&mut store);
    attach_glyphs(&mut gpu, &mut store, field_id);

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
fn steady_text_input_frame_is_allocation_free() {
    let mut h = setup_alloc();

    // Warm up until the frame path reaches steady state: the first frames grow
    // the persistent instance/mesh buffers to fit the scene, cache the
    // per-pipeline bind groups, and size the headless framebuffer/target pool.
    // A glyph-run scene needs more warmup frames than a pure-quad one before the
    // glyph instance buffer and offscreen target pool settle to a fixed capacity;
    // once settled, every steady frame allocates the same fixed amount.
    for _ in 0..32 {
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
            "frame {i}: frame_stats changed for an unchanged TextInput scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged TextInput scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged TextInput scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged TextInput scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a TextInput frame allocated a different amount on two identical steady \
         frames ({} vs {}): the paint/encode scratch is not allocation-free",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the TextInput scene must emit draw calls");
    assert!(instances > 0, "the TextInput scene must emit instances");
}
