//! A custom shader: `ripples.vs` declares a brush, `Ripple`, whose name heads
//! the record of its parameters; a `ShaderView` paints its box with one, a
//! slider drives the `rings` parameter through a state, and the rings spread
//! with the window's clock. `view!` packs the
//! shader at build time for the target's GPU backend, beside the program the
//! reference interpreter runs headless.

use viso::prelude::*;

struct Ripples;

impl Application for Ripples {
    fn new(_cx: &mut AppCx) -> Self {
        Ripples
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("ripples.vs")(cx);
    }
}

fn main() {
    viso::run::<Ripples>();
}

#[cfg(test)]
mod tests {
    use viso::gpu::{GpuBackend, HeadlessRaster, TextureDesc, TextureFormat};
    use viso::render::{Rect, Renderer};
    use viso::ui::{
        BindingTable, BuildCx, Content, EffectStore, NodeId, NodeStore, PointerButtons,
        PointerEvent, PointerPhase, PointerRouter, SemanticProjector, StateStore, TextEdits,
        VirtualLists, paint_tree, run_structure_hooks,
    };

    const W: u32 = 288;
    const H: u32 = 328;
    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

    struct Mounted {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        effects: EffectStore,
        root: NodeId,
    }

    impl Mounted {
        fn new() -> Mounted {
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
                viso::view!("ripples.vs")(&mut cx).id()
            };
            let mut mounted = Mounted {
                store,
                states,
                bindings,
                effects: EffectStore::new(),
                root,
            };
            mounted.layout();
            mounted
        }

        fn layout(&mut self) {
            let surface = Rect {
                x: 0.0,
                y: 0.0,
                w: W as f32,
                h: H as f32,
            };
            self.store.layout(self.root, surface, &mut Vec::new());
        }

        fn child(&self, at: usize) -> NodeId {
            let links = |id| self.store.arena().links(id).copied().unwrap();
            let mut child = links(self.root).first_child.unwrap();
            for _ in 0..at {
                child = links(child).next_sibling.unwrap();
            }
            child
        }

        /// The value of parameter `name` of the shader view's instance.
        fn param(&self, name: &str) -> f32 {
            let Some(Content::Shader { program, params }) =
                self.store.content_payload(self.child(0))
            else {
                panic!("the shader view paints with its shader");
            };
            let layout = &self.store.shaders().get(*program).unwrap().layout;
            let at = layout.params[layout.param(name).unwrap()].offset as usize;
            f32::from_le_bytes(params[at..at + 4].try_into().unwrap())
        }

        /// A primary click at `(x, y)`, then the state flush.
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
            let mut changed = Vec::new();
            self.states.take_pending(&mut changed);
            self.store
                .flush_state_transactions(&changed, &self.bindings);
            run_structure_hooks(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.effects,
                &changed,
            );
            self.layout();
        }

        /// The card drawn on `gpu` `time` seconds after its window opened,
        /// each program its nodes paint with installed from its code in the
        /// backend's language; its target.
        fn draw<B: GpuBackend>(&self, gpu: &mut B, time: f32) -> viso::gpu::TextureId {
            let mut renderer = Renderer::new(gpu, TextureFormat::Bgra8Unorm);
            renderer.set_surface_size([W as f32, H as f32]);
            renderer.set_time(time);
            for (slot, program) in self.store.shaders().iter() {
                renderer
                    .install_brush(gpu, slot, program)
                    .unwrap_or_else(|e| panic!("{e}"));
            }
            let mut prims = Vec::new();
            paint_tree(&self.store, self.root, &mut prims);
            renderer.upload(gpu, &prims);
            assert!(renderer.reads_time(), "the ripples keep frames coming");
            let target = gpu.create_texture(&TextureDesc {
                width: W,
                height: H,
                format: TextureFormat::Bgra8Unorm,
                render_target: true,
                label: "ripples",
            });
            renderer.render_to_texture(gpu, target, WHITE, [W as f32, H as f32]);
            assert_eq!(renderer.frame_stats().program_draws_dropped, 0);
            target
        }

        fn headless(&self, time: f32) -> Vec<u8> {
            let mut gpu = HeadlessRaster::new();
            let target = self.draw(&mut gpu, time);
            gpu.read_texture(target)
        }
    }

    /// The BGRA8 pixel at `(x, y)` of a `W`-wide picture.
    fn pixel(picture: &[u8], x: u32, y: u32) -> [u8; 4] {
        let at = ((y * W + x) * 4) as usize;
        picture[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn the_ripples_paint_their_box_and_the_slider_moves_the_rings() {
        let mut card = Mounted::new();
        assert_eq!(card.param("rings"), 6.0);
        let picture = card.headless(0.0);
        // The box spans 0..288 on both axes; its center is the tint at full
        // strength, the white canvas shows in the gap below it.
        let center = pixel(&picture, 144, 144);
        assert!(center[0] > 200 && center[2] < 100, "{center:?}");
        assert_eq!(pixel(&picture, 144, 296), [255, 255, 255, 255]);
        // Past the fade the box is black.
        assert_eq!(pixel(&picture, 4, 4), [0, 0, 0, 255]);

        // The slider spans x 0..288 at y 304..328: a click three quarters of
        // the way across sets three quarters of its 1..16 range.
        card.click(288.0 * 0.75, 316.0);
        assert_eq!(card.param("rings"), 1.0 + 15.0 * 0.75);
        assert_ne!(card.headless(0.0), picture, "the rings moved");
    }

    #[test]
    fn the_rings_spread_with_the_clock() {
        let card = Mounted::new();
        let start = card.headless(0.0);
        let later = card.headless(0.5);
        assert_ne!(later, start);
        // In half a second the crest moves a quarter of a ring out from the
        // center, which falls to about half the tint.
        let (b0, b1) = (pixel(&start, 144, 144)[0], pixel(&later, 144, 144)[0]);
        assert!(b0 >= 250 && (115..=150).contains(&b1), "{b0} {b1}");
        // A whole period, two seconds, later the picture is the same again.
        let again = card.headless(2.0);
        assert!(again.iter().zip(&start).all(|(a, b)| a.abs_diff(*b) <= 1));
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_ripples_match_the_reference_interpreter_on_metal() {
        let card = Mounted::new();
        let reference = card.headless(0.75);
        let mut metal = viso::gpu::MetalBackend::new();
        let target = card.draw(&mut metal, 0.75);
        let pixels = metal.read_texture(target);
        let over = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 2))
            .count();
        assert_eq!(over, 0, "pixels off by more than 2/255");
    }
}
