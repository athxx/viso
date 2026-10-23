//! A view's `Chip`: one that selects flips its `selected` on a release or
//! Space, one that removes reports `removed` on a release on its cross or
//! Delete and clicks elsewhere, and a selected chip widens for its tick.

use viso::render::Rect;
use viso::ui::{
    BindingTable, BuildCx, CheckMark, CheckShape, EffectStore, Key, KeyEvent, KeyRouter, NodeId,
    NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Role, SemanticProjector,
    SemanticState, StateStore, TextEdits, VirtualLists, focus_node, run_structure_hooks,
};

viso::component! {
    Chips {
        state veggie: Bool = false;
        state removed: I32 = 0;
        state clicks: I32 = 0;
        view {
            Column {
                width: 300dp;
                height: 200dp;
                Chip { label: "Vegetarian"; bind selected <=> veggie; }
                Chip {
                    label: "rust";
                    removable: true;
                    on removed { removed += 1; }
                    on click { clicks += 1; }
                }
                Badge { count: Option::Some(removed); }
                Badge { count: Option::Some(clicks); }
            }
        }
    }
}

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 400.0,
    h: 300.0,
};

struct Mounted {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
}

impl Mounted {
    fn new() -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            Chips::build(&mut cx).1.id()
        };
        store.layout(root, SURFACE, &mut Vec::new());
        Self {
            store,
            states,
            bindings,
            root,
        }
    }

    fn child(&self, n: usize) -> NodeId {
        self.store.arena().children(self.root).nth(n).unwrap()
    }

    fn text(&self, n: usize) -> String {
        self.store.text_request(self.child(n)).unwrap().text.clone()
    }

    fn flush(&mut self) {
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut EffectStore::new(),
            &mut VirtualLists::new(),
            &changed,
        );
    }

    fn click(&mut self, x: f32, y: f32) {
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x,
                y,
                phase,
                buttons: PointerButtons::PRIMARY,
                modifiers: Default::default(),
            };
            PointerRouter::route(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                self.root,
                event,
                &mut chain,
            );
        }
        self.flush();
    }

    fn key(&mut self, node: NodeId, key: Key) {
        focus_node(&mut self.store, node);
        let ev = KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut TextEdits::new(),
            self.root,
            ev,
            &mut Vec::new(),
        );
        self.flush();
    }
}

#[test]
fn a_filter_chip_flips_its_selection_and_widens_for_its_tick() {
    let mut chips = Mounted::new();
    let veggie = chips.child(0);
    let narrow = chips.store.bounds(veggie);
    assert_eq!(narrow.h, 32.0);
    assert_eq!(
        chips.store.check_mark(veggie),
        Some(CheckMark {
            shape: CheckShape::Chip { removable: false },
            checked: false,
        })
    );
    assert_eq!(chips.store.semantics(veggie).unwrap().role, Role::CheckBox);

    chips.click(narrow.x + 4.0, narrow.y + 4.0);
    assert!(chips.store.check_mark(veggie).unwrap().checked);
    assert_eq!(
        chips.store.semantic_state(veggie),
        Some(SemanticState::checked(true))
    );
    chips
        .store
        .tick_transitions(core::time::Duration::from_secs(1));
    chips.store.layout(chips.root, SURFACE, &mut Vec::new());
    assert_eq!(
        chips.store.bounds(veggie).w,
        narrow.w + 22.0,
        "room for the tick"
    );

    chips.key(veggie, Key::Space);
    assert!(!chips.store.check_mark(veggie).unwrap().checked);
}

#[test]
fn an_input_chip_removes_from_its_cross_or_delete_and_clicks_elsewhere() {
    let mut chips = Mounted::new();
    let rust = chips.child(1);
    assert_eq!(chips.store.semantics(rust).unwrap().role, Role::Button);
    let at = chips.store.bounds(rust);
    // The cross takes the chip's last 34.
    chips.click(at.x + at.w - 10.0, at.y + 10.0);
    assert_eq!((chips.text(2), chips.text(3)), ("1".into(), "0".into()));
    chips.click(at.x + 4.0, at.y + 10.0);
    assert_eq!((chips.text(2), chips.text(3)), ("1".into(), "1".into()));
    chips.key(rust, Key::Delete);
    chips.key(rust, Key::Enter);
    assert_eq!((chips.text(2), chips.text(3)), ("2".into(), "2".into()));
}

// --- golden screenshot -------------------------------------------------------

mod golden {
    use std::path::PathBuf;

    use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle, TextureDesc, TextureFormat};
    use viso::render::{GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs};
    use viso::ui::{
        Axis, BindingTable, BoxStyle, BuildCx, Component, Content, Inset, NodeId, NodeStore,
        SemanticProjector, Size, StateStore, TextEdits, Vec2, VirtualLists, paint_tree,
    };
    use viso::widgets::{ViewStyle, chip, view};

    const W: u32 = 260;
    const H: u32 = 156;
    const TOL: u8 = 2;

    const DARK: Rgba = Rgba {
        r: 0.1,
        g: 0.1,
        b: 0.12,
        a: 1.0,
    };

    fn natural(glyphs: &[GlyphInstanceData]) -> Vec2 {
        glyphs.iter().fold(Vec2::ZERO, |n, g| Vec2 {
            x: n.x.max(g.rect.x + g.rect.w),
            y: n.y.max(g.rect.y + g.rect.h),
        })
    }

    /// A dark column holding an unselected chip, a selected one and a
    /// removable one, each captioned by the deterministic glyph run.
    fn scene(gpu: &mut CpuRaster, store: &mut NodeStore) -> NodeId {
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let container = view(ViewStyle {
            axis: Axis::Column,
            padding: Inset::all(4.0),
            gap: 4.0,
            size: Size::fill(),
            background: BoxStyle::solid(DARK),
            ..Default::default()
        })
        .children(|cx| {
            chip("a").selected(false).build(cx);
            chip("b").selected(true).build(cx);
            chip("c").on_remove(|_| {}).build(cx);
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
            cx.root().unwrap()
        };
        let tg = test_glyphs([0.0, 0.0], 14.0);
        let atlas = gpu.create_texture(&TextureDesc {
            width: tg.atlas_size,
            height: tg.atlas_size,
            format: TextureFormat::R8Unorm,
            render_target: false,
            label: "chip-caption-atlas",
        });
        gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);
        let size = natural(&tg.glyphs);
        let chips: Vec<_> = store.arena().children(root).collect();
        for chip in chips {
            store.set_content_payload(
                chip,
                Content::Text {
                    ink: None,
                    glyphs: tg.glyphs.clone(),
                    atlas,
                    color_glyphs: Vec::new(),
                    color_atlas: None,
                    color: Rgba::new(1.0, 1.0, 1.0, 1.0),
                    natural: size,
                    baseline: 0.0,
                    shaped_at_width: None,
                    width_use: viso::ui::WidthUse::Ignored,
                    min_content: size.x,
                    max_content: size.x,
                },
            );
        }
        root
    }

    #[test]
    fn chips_draw_their_bodies_ticks_and_crosses_and_match_golden() {
        let mut gpu = CpuRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut renderer = Renderer::new(&mut gpu, format);
        let mut store = NodeStore::new();
        let root = scene(&mut gpu, &mut store);
        let rect = Rect::new(0.0, 0.0, W as f32, H as f32);
        store.layout(root, rect, &mut Vec::new());
        let sizes: Vec<Rect> = store
            .arena()
            .children(root)
            .map(|c| store.bounds(c))
            .collect();
        // The tick takes 0.6875 of the height, the cross 0.6875 more than
        // the room it replaces.
        let room = sizes[0].h * 0.6875;
        assert!((sizes[1].w - sizes[0].w - room).abs() < 1e-3, "{sizes:?}");
        assert!((sizes[2].w - sizes[0].w - room).abs() < 1e-3, "{sizes:?}");

        let mut primitives = Vec::new();
        paint_tree(&store, root, &mut primitives);
        renderer.upload(&mut gpu, &primitives);
        renderer.submit(
            &mut gpu,
            surface,
            [0.0, 0.0, 0.0, 1.0],
            [W as f32, H as f32],
        );
        let actual = gpu.read_pixels_bgra8(surface);

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/chips.bgra8");
        if std::env::var("BLESS").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &actual).unwrap();
            return;
        }
        let expected = std::fs::read(&path)
            .unwrap_or_else(|_| panic!("missing golden {}; run with BLESS=1", path.display()));
        assert_eq!(actual.len(), expected.len());
        let worst = actual
            .iter()
            .zip(&expected)
            .map(|(a, e)| a.abs_diff(*e))
            .max()
            .unwrap_or(0);
        assert!(
            worst <= TOL,
            "golden mismatch: max per-channel diff {worst}"
        );
    }
}
