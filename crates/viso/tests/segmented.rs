//! A view's `SegmentedControl`: its caption items are segments, centred with
//! room round them over a track; a release on one or an arrow selects it and
//! the thumb slides from where it showed to it; the widget draws the same.

use std::time::Duration;

use viso::render::Rect;
use viso::ui::{
    BindingTable, BuildCx, CheckShape, EffectStore, Key, KeyEvent, KeyRouter, NodeId, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, PointerRouter, Role, SemanticProjector, StateStore,
    TextEdits, VirtualLists, focus_node, run_structure_hooks,
};

viso::component! {
    Range {
        state range: U32 = 0;
        view {
            Column {
                width: 300dp;
                height: 200dp;
                SegmentedControl {
                    width: 240dp;
                    bind selected <=> range;
                    Text { text: "Day"; }
                    Text { text: "Week"; }
                    Text { text: "Month"; }
                }
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
            Range::build(&mut cx).1.id()
        };
        store.layout(root, SURFACE, &mut Vec::new());
        Self {
            store,
            states,
            bindings,
            root,
        }
    }

    fn control(&self) -> NodeId {
        self.store.arena().first_child(self.root).unwrap()
    }

    fn segment(&self, n: usize) -> NodeId {
        self.store.arena().children(self.control()).nth(n).unwrap()
    }

    fn selected(&self) -> Option<u32> {
        self.store.segments(self.control()).unwrap().selected
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

    fn key(&mut self, key: Key) {
        let control = self.control();
        focus_node(&mut self.store, control);
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

    /// The thumb's box, relative to the control.
    fn thumb(&self) -> Rect {
        self.store.thumb_at(self.control()).unwrap()
    }

    /// Segment `n`'s box, relative to the control.
    fn local(&self, n: usize) -> Rect {
        let (b, o) = (
            self.store.bounds(self.segment(n)),
            self.store.bounds(self.control()),
        );
        Rect::new(b.x - o.x, b.y - o.y, b.w, b.h)
    }
}

#[test]
fn caption_items_are_radio_segments_sharing_the_track() {
    let view = Mounted::new();
    let control = view.control();
    let track = view.store.bounds(control);
    assert_eq!(track.w, 240.0);
    assert!(view.store.segments(control).is_some());
    for n in 0..3 {
        let segment = view.segment(n);
        assert_eq!(
            view.store.check_mark(segment).map(|m| m.shape),
            Some(CheckShape::Segment)
        );
        assert_eq!(view.store.semantics(segment).unwrap().role, Role::Radio);
        let b = view.local(n);
        assert_eq!(b.w, 236.0 / 3.0, "the segments share the track");
        assert_eq!(b.y, 2.0, "the track keeps room round its segments");
        assert!(b.h >= 28.0);
    }
    assert_eq!(view.selected(), Some(0));
    assert_eq!(view.thumb(), view.local(0), "the thumb sits on the first");
}

#[test]
fn a_release_or_an_arrow_selects_and_the_thumb_slides_there() {
    let mut view = Mounted::new();
    let week = view.store.bounds(view.segment(1));
    view.click(week.x + week.w * 0.5, week.y + week.h * 0.5);
    assert_eq!(view.selected(), Some(1));
    assert!(view.store.segments(view.control()).unwrap().shown() < 1.0);
    view.store.tick_transitions(Duration::from_millis(60));
    let moving = view.thumb();
    assert!(
        moving.x > view.local(0).x && moving.x < view.local(1).x,
        "{moving:?} is on the way"
    );
    view.store.tick_transitions(Duration::from_secs(2));
    assert_eq!(view.thumb(), view.local(1));
    assert_eq!(
        view.store
            .semantic_state(view.segment(1))
            .and_then(|s| s.selected),
        Some(true)
    );

    view.key(Key::End);
    assert_eq!(view.selected(), Some(2));
    view.store.tick_transitions(Duration::from_secs(2));
    assert_eq!(view.thumb(), view.local(2));
}

// --- golden screenshot -------------------------------------------------------

mod golden {
    use std::path::PathBuf;

    use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle, TextureDesc, TextureFormat};
    use viso::render::{GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs};
    use viso::ui::{
        Axis, BindingTable, BoxStyle, BuildCx, Component, Content, Inset, Length, NodeId,
        NodeStore, SemanticProjector, Sizing, StateStore, TextEdits, Vec2, VirtualLists,
        paint_tree,
    };
    use viso::widgets::{ViewStyle, segmented_control, view};

    const W: u32 = 260;
    const H: u32 = 136;
    const TOL: u8 = 2;

    fn natural(glyphs: &[GlyphInstanceData]) -> Vec2 {
        glyphs.iter().fold(Vec2::ZERO, |n, g| Vec2 {
            x: n.x.max(g.rect.x + g.rect.w),
            y: n.y.max(g.rect.y + g.rect.h),
        })
    }

    /// A dark column holding a three-segment control with its middle
    /// selected, each captioned by the deterministic glyph run.
    fn scene(gpu: &mut CpuRaster, store: &mut NodeStore) -> NodeId {
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let container = view(ViewStyle {
            axis: Axis::Column,
            padding: Inset::all(8.0),
            gap: 8.0,
            size: Sizing::fill(),
            background: BoxStyle::solid(Rgba::new(0.1, 0.1, 0.12, 1.0)),
            ..Default::default()
        })
        .children(|cx| {
            let width = Sizing {
                width: Length::Fixed(240.0),
                height: Length::Fit,
            };
            segmented_control(["a", "b", "c"])
                .selected(1)
                .size(width)
                .build(cx);
            segmented_control(["a", "b", "c", "d"])
                .size(width)
                .build(cx);
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
            label: "segment-caption-atlas",
        });
        gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);
        let size = natural(&tg.glyphs);
        let controls: Vec<_> = store.arena().children(root).collect();
        for control in controls {
            let segments: Vec<_> = store.arena().children(control).collect();
            for segment in segments {
                store.set_content_payload(
                    segment,
                    Content::Text {
                        ink: None,
                        glyphs: tg.glyphs.clone(),
                        atlas,
                        lane: viso::render::GlyphLane::CoverageA8,
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
        }
        root
    }

    #[test]
    fn segments_share_the_track_under_a_thumb_and_match_golden() {
        let mut gpu = CpuRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut renderer = Renderer::new(&mut gpu, format);
        let mut store = NodeStore::new();
        let root = scene(&mut gpu, &mut store);
        store.layout(
            root,
            Rect::new(0.0, 0.0, W as f32, H as f32),
            &mut Vec::new(),
        );
        let first = store.arena().first_child(root).unwrap();
        let widths: Vec<f32> = store
            .arena()
            .children(first)
            .map(|s| store.bounds(s).w)
            .collect();
        assert_eq!(widths, [236.0 / 3.0; 3], "the segments share the track");

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
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/segmented.bgra8");
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
