//! The renderer's 3D scene API: frames handed to scene slots, uploaded meshes,
//! and the scene draws of the stream resolved to images of their targets.

use viso_gpu::GpuBackend;

use super::Renderer;
use crate::primitive::{Primitive, SceneSlot};
use crate::scene::bounds::Bounds;
use crate::scene::ingest::Instance;
use crate::scene3d::{MeshId, SceneFrame, SceneStats, SceneVertex};

/// The largest scene target edge, in pixels.
const MAX_TARGET: u32 = 8192;

impl Renderer {
    /// Hand `frame` to the scene in `slot`, taking back the frame the slot
    /// held (empty for a new slot) in its place: refill that one for the next
    /// frame, so a steady scene neither copies nor allocates.
    ///
    /// The next upload that draws the slot renders the new frame. Its
    /// [`SceneDraw`](crate::SceneDraw) must carry a new revision so the rect
    /// it composites into redraws.
    pub fn swap_scene(&mut self, slot: SceneSlot, frame: &mut SceneFrame) {
        self.scene3d.swap(slot, frame);
    }

    /// Free the scene in `slot`: its frame, targets and buffers.
    pub fn release_scene<B: GpuBackend>(&mut self, backend: &mut B, slot: SceneSlot) {
        if let Some(color) = self.scene3d.release(backend, slot) {
            self.release_texture(backend, color);
        }
    }

    /// Upload a mesh of `vertices` and counter-clockwise triangle `indices`
    /// for scenes to draw by the returned id.
    pub fn create_scene_mesh<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        vertices: &[SceneVertex],
        indices: &[u32],
    ) -> MeshId {
        self.scene3d.create_mesh(backend, vertices, indices)
    }

    /// Destroy a mesh [`create_scene_mesh`](Self::create_scene_mesh)
    /// uploaded. Its id may be handed out again; a frame that still names it
    /// draws whatever mesh holds the id then.
    pub fn release_scene_mesh<B: GpuBackend>(&mut self, backend: &mut B, mesh: MeshId) {
        self.scene3d.release_mesh(backend, mesh);
    }

    /// The scene lane's counters since they were last taken.
    pub fn take_scene_stats(&mut self) -> SceneStats {
        self.scene3d.take_stats()
    }

    /// Size and fill the scene of every scene draw among `primitives`.
    pub(super) fn place_scenes<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        primitives: &[Primitive],
    ) {
        let max = backend.caps().max_texture_size.clamp(1, MAX_TARGET);
        for prim in primitives {
            if let Primitive::Scene(draw) = prim
                && let Some(retired) = self.scene3d.place(backend, draw, self.scale, max)
            {
                self.release_texture(backend, retired);
            }
        }
    }

    /// `prim`'s one instance with `fold` applied and its bounds, a scene
    /// draw resolved to the image of its target, or `None` for a kind that is
    /// not one instance in one store.
    pub(super) fn leaf_instance(&self, prim: &Primitive, fold: f32) -> Option<(Instance, Bounds)> {
        match prim {
            Primitive::Scene(draw) => {
                let image = self.scene3d.image(draw)?;
                Some(Instance::image(&image, draw.revision, fold))
            }
            _ => Instance::of(prim, fold),
        }
    }
}
