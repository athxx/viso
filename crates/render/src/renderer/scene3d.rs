//! The renderer's 3D scene API: frames handed to scene slots, uploaded meshes,
//! and the scene draws of the stream resolved to images of their targets.

use viso_gpu::GpuBackend;

use super::Renderer;
use crate::primitive::{Primitive, SceneSlot};
use crate::scene::bounds::Bounds;
use crate::scene::ingest::Instance;
use crate::scene3d::SceneLane;
use crate::scene3d::model::ModelAsset;
use crate::scene3d::{
    Material, MaterialId, MeshId, SceneFrame, SceneModel, SceneStats, SceneVertex, SkinnedVertex,
    VertexExtra,
};

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

    /// Upload a skinned mesh of `vertices` in their bind pose and
    /// counter-clockwise triangle `indices`, for scenes to draw by the
    /// returned id with [`SceneFrame::push_skinned`]. Released like any
    /// mesh, with [`release_scene_mesh`](Self::release_scene_mesh).
    pub fn create_scene_skinned_mesh<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        vertices: &[SkinnedVertex],
        indices: &[u32],
    ) -> MeshId {
        self.scene3d.create_skinned_mesh(backend, vertices, indices)
    }

    /// [`create_scene_mesh`](Self::create_scene_mesh) of vertices that each
    /// carry their extra in `extras`, by index (the default past its end): a
    /// second texture coordinate set the material's maps may read and a
    /// color multiplying its base color. Each vertex takes 64 bytes rather
    /// than 48, and only such a mesh's draws read them.
    pub fn create_scene_mesh_extra<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        vertices: &[SceneVertex],
        extras: &[VertexExtra],
        indices: &[u32],
    ) -> MeshId {
        self.scene3d
            .create_mesh_extra(backend, vertices, extras, indices)
    }

    /// [`create_scene_skinned_mesh`](Self::create_scene_skinned_mesh) of
    /// vertices that each carry their extra in `extras`, as
    /// [`create_scene_mesh_extra`](Self::create_scene_mesh_extra)'s do: 80
    /// bytes a vertex rather than 64.
    pub fn create_scene_skinned_mesh_extra<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        vertices: &[SkinnedVertex],
        extras: &[VertexExtra],
        indices: &[u32],
    ) -> MeshId {
        self.scene3d
            .create_skinned_mesh_extra(backend, vertices, extras, indices)
    }

    /// Destroy a mesh [`create_scene_mesh`](Self::create_scene_mesh)
    /// uploaded. Its id may be handed out again; a frame that still names it
    /// draws whatever mesh holds the id then.
    pub fn release_scene_mesh<B: GpuBackend>(&mut self, backend: &mut B, mesh: MeshId) {
        self.scene3d.release_mesh(backend, mesh);
    }

    /// Make a material for scenes to draw instances in with
    /// [`SceneFrame::push_with`].
    pub fn create_scene_material<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        material: &Material,
    ) -> MaterialId {
        let uniforms = SceneLane::uniforms_of(material);
        let sampler = viso_gpu::SamplerDesc::LINEAR_CLAMP;
        self.scene3d
            .create_material(backend, &uniforms, [None; 5], sampler)
    }

    /// Destroy a material [`create_scene_material`](Self::create_scene_material)
    /// made. Its id may be handed out again; a frame that still names it
    /// draws in whatever material holds the id then. A built-in material
    /// stays.
    pub fn release_scene_material<B: GpuBackend>(&mut self, backend: &mut B, material: MaterialId) {
        self.scene3d.release_material(backend, material);
    }

    /// Upload `asset`'s geometry, materials and images for scenes to draw
    /// with [`SceneFrame::push_model`]. The asset may be let go of
    /// afterwards: the model keeps only its rig.
    pub fn create_scene_model<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        asset: &ModelAsset,
    ) -> SceneModel {
        SceneModel::upload(asset, &mut self.scene3d, backend)
    }

    /// Destroy what [`create_scene_model`](Self::create_scene_model)
    /// uploaded for `model`. A frame that still draws it draws whatever
    /// takes its meshes' and materials' ids next.
    pub fn release_scene_model<B: GpuBackend>(&mut self, backend: &mut B, model: &SceneModel) {
        model.release(&mut self.scene3d, backend);
    }

    /// The scene lane's counters since they were last taken.
    pub fn take_scene_stats(&mut self) -> SceneStats {
        self.scene3d.take_stats()
    }

    /// Sizing and fill the scene of every scene draw among `primitives`.
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
