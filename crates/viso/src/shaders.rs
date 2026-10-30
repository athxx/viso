//! The brush programs a window's nodes paint with, installed into its
//! renderer.
//!
//! The node store keeps each program by slot; a window's renderer keeps a
//! pipeline per slot. Once a frame, after the settle delivered the frame's
//! values and before layout, [`InstalledShaders::sync`] brings the renderer to
//! what the slots hold — only when their revision moved: a slot whose program
//! changed is installed (compiled from the code the build packed for this
//! backend), one emptied is released through the backend's fence, so a frame
//! in flight finishes with the old pipeline. A program the backend refuses is
//! reported once and its slot draws nothing. A hot reload compiles the
//! programs its patch brings off the loop and hands them to the renderer
//! before it commits, refusing the patch when one does not build, so the sync
//! after it only installs.

use std::sync::Arc;

use viso_gpu::GpuBackend;
use viso_render::{BrushProgram, ProgramSlot, Renderer};
use viso_ui::ShaderSlots;

/// What a window's renderer holds of its store's program slots.
#[derive(Debug, Default)]
pub(crate) struct InstalledShaders {
    /// The revision of the slots last synced; `None` before the first sync.
    revision: Option<u64>,
    /// The program each slot was last brought to, installed or refused.
    slots: Vec<Option<Arc<BrushProgram>>>,
}

impl InstalledShaders {
    /// Installs into `renderer` each slot of `shaders` whose program changed
    /// since the last sync and releases each emptied; nothing when their
    /// revision did not move.
    pub(crate) fn sync<B: GpuBackend>(
        &mut self,
        shaders: &ShaderSlots,
        backend: &mut B,
        renderer: &mut Renderer,
    ) {
        if self.revision == Some(shaders.revision()) {
            return;
        }
        self.revision = Some(shaders.revision());
        if self.slots.len() < shaders.len() {
            self.slots.resize(shaders.len(), None);
        }
        for (at, held) in self.slots.iter_mut().enumerate() {
            let slot = ProgramSlot(at as u32);
            match (shaders.get(slot), held.as_ref()) {
                (Some(next), Some(prior)) if Arc::ptr_eq(next, prior) => {}
                (Some(next), _) => {
                    if let Err(error) = renderer.install_brush(backend, slot, next) {
                        eprintln!("[viso] shader `{}` does not install: {error}", next.name);
                        renderer.release_program(backend, slot);
                    }
                    *held = Some(Arc::clone(next));
                }
                (None, Some(_)) => {
                    renderer.release_program(backend, slot);
                    *held = None;
                }
                (None, None) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use viso_dsl::aot::pack_brush;
    use viso_dsl::frontend::{Origin, compile_file};
    use viso_dsl::view_behavior::ViewBrush;
    use viso_gpu::{CpuRaster, ShaderLang, TextureFormat};
    use viso_ui::reactive::EffectStore;
    use viso_ui::{BuildCx, FlexStyle, LeafStyle, NodeStore};

    use super::*;

    const FILL: &str = r#"
shader Fill {
    instance color: ColorLinear;
    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        return VertexOutput {
            clip_position: to_clip(rect_pos + unit * rect_size, viewport_size),
        };
    }
    fragment() -> ColorLinear {
        return color;
    }
}
"#;

    /// `Fill`, packed in `langs` and loaded as a program.
    fn fill(langs: &[ShaderLang]) -> Arc<BrushProgram> {
        let origin = Origin {
            package: "app".to_owned(),
            module: vec!["main".to_owned()],
            language: None,
            ..Default::default()
        };
        let compiled = compile_file(FILL, &origin);
        let shader = &compiled.shaders[0];
        let brush = ViewBrush {
            program: shader.program.clone().expect("a program"),
            layout: shader.brush.clone().expect("a brush"),
        };
        viso_view::load_shader(&pack_brush(&brush, langs).unwrap()).unwrap()
    }

    #[test]
    fn a_window_installs_the_programs_its_nodes_paint_with_and_releases_the_unused() {
        let mut gpu = CpuRaster::new();
        let mut renderer = Renderer::new(&mut gpu, TextureFormat::Bgra8Unorm);
        let mut installed = InstalledShaders::default();
        let mut store = NodeStore::new();
        let (mut a, mut b) = (None, None);
        {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                for node in [&mut a, &mut b] {
                    *node = Some(cx.leaf(LeafStyle::default()).id());
                }
            });
        }
        let (a, b) = (a.unwrap(), b.unwrap());
        let program = fill(&[ShaderLang::None]);
        let params: Arc<[u8]> = vec![0; program.layout.stride as usize].into();
        store.set_shader(a, &program, Arc::clone(&params), &[]);
        store.set_shader(b, &program, Arc::clone(&params), &[]);
        installed.sync(store.shaders(), &mut gpu, &mut renderer);
        let pipeline = renderer.program_pipeline(ProgramSlot(0));
        assert!(pipeline.is_some(), "installed once for both nodes");

        // A frame whose slots did not move installs nothing.
        installed.sync(store.shaders(), &mut gpu, &mut renderer);
        assert_eq!(renderer.program_pipeline(ProgramSlot(0)), pipeline);

        // Freed with its last node, the program leaves the renderer.
        let mut effects = EffectStore::default();
        store.free_tree(a, &mut effects, &mut Vec::new());
        installed.sync(store.shaders(), &mut gpu, &mut renderer);
        assert!(renderer.program_pipeline(ProgramSlot(0)).is_some());
        store.free_tree(b, &mut effects, &mut Vec::new());
        installed.sync(store.shaders(), &mut gpu, &mut renderer);
        assert!(renderer.program_pipeline(ProgramSlot(0)).is_none());

        // Painted with again, it draws with the pipeline its code kept.
        let compiles = renderer.program_compiles();
        let again = BuildCx::new(&mut store).leaf(LeafStyle::default()).id();
        store.set_shader(again, &program, Arc::clone(&params), &[]);
        installed.sync(store.shaders(), &mut gpu, &mut renderer);
        assert_eq!(renderer.program_pipeline(ProgramSlot(0)), pipeline);
        assert_eq!(renderer.program_compiles(), compiles);
        store.free_tree(again, &mut effects, &mut Vec::new());
        installed.sync(store.shaders(), &mut gpu, &mut renderer);

        // A program without this backend's code draws nothing.
        let c = BuildCx::new(&mut store).leaf(LeafStyle::default()).id();
        let metal = fill(&[ShaderLang::Msl]);
        store.set_shader(c, &metal, params, &[]);
        installed.sync(store.shaders(), &mut gpu, &mut renderer);
        assert!(renderer.program_pipeline(ProgramSlot(0)).is_none());
    }
}
