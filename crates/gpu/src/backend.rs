//! The trait-based RHI: a single source-level GPU interface (§17.2).
//!
//! There is exactly one `GpuBackend` trait; the concrete backend is chosen at
//! compile time by [`create_device`] and the facade holds it monomorphized, so
//! the frame hot path has no `dyn GpuBackend` dispatch (ADR-007). The trait
//! exists to keep the Metal and headless-raster backends *source-compatible* and
//! to let cold-path code (setup, tests) be backend-generic.
//!
//! The backend consumes a low-level [`DrawList`] — a flat sequence of
//! [`DrawCommand`]s that `viso-render` lowers its frame packet into. This keeps
//! the DAG edge one-way: `viso-render → viso-gpu` (render knows about draw
//! commands; gpu never knows about primitives, batches, or widgets).

use viso_handle::RawWindowHandle;

use crate::instance::InstanceLayout;
use crate::resource::{
    BindGroupDesc, BufferDesc, Caps, PipelineDesc, SamplerDesc, TextureDesc, TextureFormat,
};
use crate::{BindGroupId, BufferId, PipelineId, SamplerId, SurfaceId, TextureId};

/// A frame acquired from a surface: an opaque, backend-specific token that must
/// be handed back to [`GpuBackend::present`]. Carried by value so the borrow
/// checker enforces "one present per begin_frame".
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// The surface this frame belongs to.
    pub surface: SurfaceId,
    /// Backend-defined index of the acquired drawable (e.g. Metal drawable slot).
    pub drawable: u32,
}

/// A load action for a render pass's color attachment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadOp {
    /// Clear to the given premultiplied RGBA color before drawing.
    Clear([f32; 4]),
    /// Preserve existing contents.
    Load,
}

/// The target a render pass draws into.
#[derive(Debug, Clone, Copy)]
pub enum RenderTarget {
    /// The acquired swapchain frame.
    Surface(Frame),
    /// An offscreen texture (layer / composite pass).
    Texture(TextureId),
}

/// A render pass: one target, one load action, and a contiguous range of draw
/// commands inside the frame's flat [`DrawList::commands`] buffer.
///
/// The commands are referenced by range rather than by a borrowed slice so a
/// pass carries no lifetime and can live in a renderer-owned scratch buffer that
/// is cleared and refilled each frame (0 steady-state heap allocations, §7.1).
#[derive(Debug, Clone, Copy)]
pub struct RenderPass {
    /// Where this pass renders.
    pub target: RenderTarget,
    /// What to do with the target's prior contents.
    pub load: LoadOp,
    /// Index of this pass's first command in [`DrawList::commands`].
    pub first_command: u32,
    /// Number of commands this pass draws, starting at `first_command`.
    pub command_count: u32,
}

impl RenderPass {
    /// This pass's command range into [`DrawList::commands`].
    #[inline]
    pub fn command_range(&self) -> core::ops::Range<usize> {
        let start = self.first_command as usize;
        start..start + self.command_count as usize
    }
}

/// Inline per-draw uniform bytes, stored by value so a [`DrawCommand`] carries no
/// borrow. The backends read [`Self::as_bytes`] and hand it to Metal's
/// `setVertexBytes`/`setFragmentBytes` (headless ignores it), preserving the
/// arbitrary-length inline-byte uniform semantics — capped at [`Self::MAX`] bytes,
/// which comfortably fits the built-ins' viewport uniform.
#[derive(Debug, Clone, Copy)]
pub struct InlineUniforms {
    bytes: [u8; Self::MAX],
    len: u8,
}

impl InlineUniforms {
    /// Maximum inline uniform payload in bytes.
    pub const MAX: usize = 16;

    /// Empty uniforms (no inline bytes bound).
    pub const EMPTY: Self = Self {
        bytes: [0; Self::MAX],
        len: 0,
    };

    /// Build inline uniforms from `src`.
    ///
    /// # Panics
    /// Panics if `src.len() > Self::MAX`.
    #[inline]
    pub fn new(src: &[u8]) -> Self {
        assert!(
            src.len() <= Self::MAX,
            "inline uniform payload {} exceeds {} bytes",
            src.len(),
            Self::MAX
        );
        let mut bytes = [0u8; Self::MAX];
        bytes[..src.len()].copy_from_slice(src);
        Self {
            bytes,
            len: src.len() as u8,
        }
    }

    /// The inline uniform bytes.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// Whether there are any inline uniform bytes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// How a [`DrawCommand`] sources its geometry — the two shapes Viso draws.
///
/// The built-in rect primitives (Quad/Image/GlyphRun) carry no vertex buffer:
/// the vertex shader synthesizes a unit quad from `vertex_id` and instances it,
/// so their geometry is [`Geometry::Generated`]. Vector primitives (Path/Mesh)
/// are CPU-tessellated into a real vertex + index buffer and drawn once as an
/// indexed triangle list — [`Geometry::IndexedMesh`]. Splitting the two keeps
/// the backend's encode path explicit instead of dispatching on the pipeline's
/// built-in tag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Geometry {
    /// A `vertex_id`-generated unit quad, instanced. The per-instance data lives
    /// in [`DrawCommand::instance_buffer`] at `instance_offset`; `count` is the
    /// number of instances (6 vertices each).
    Generated {
        /// Number of instances to draw.
        count: u32,
    },
    /// A real vertex buffer (bound at index 0) drawn as one indexed triangle
    /// list of `index_count` indices. There is no per-instance data; per-vertex
    /// attributes (position, color, AA edge) come from the vertex buffer.
    IndexedMesh {
        /// The per-vertex geometry buffer, bound at index 0.
        vertex_buffer: BufferId,
        /// The `u32` index buffer.
        index_buffer: BufferId,
        /// Offset (in indices) of this draw's first index into `index_buffer`.
        /// Multiple mesh draws (e.g. different clips) share one index buffer;
        /// each starts at its own offset.
        index_offset: u32,
        /// Number of indices to draw (3 per triangle).
        index_count: u32,
    },
}

/// A single draw call: the lowered form of one render batch.
///
/// [`Self::geometry`] selects the geometry source: `vertex_id`-generated
/// instanced quads, or a real indexed vertex/index buffer for vector meshes.
/// Uniforms are passed inline as bytes (Metal `setVertexBytes`/`setFragmentBytes`).
#[derive(Debug, Clone, Copy)]
pub struct DrawCommand {
    /// The pipeline (shader + blend + formats) for this draw.
    pub pipeline: PipelineId,
    /// Optional bind group (textures + samplers + uniform buffers).
    pub bind_group: Option<BindGroupId>,
    /// Where this draw's geometry comes from (generated quads vs. indexed mesh).
    pub geometry: Geometry,
    /// Per-instance data buffer, bound at index 1 (used by
    /// [`Geometry::Generated`]; ignored for indexed meshes).
    pub instance_buffer: BufferId,
    /// Byte offset into the instance buffer for the first instance of this draw.
    pub instance_offset: usize,
    /// Inline uniform bytes for this draw (bound at a fixed uniform index),
    /// stored by value so the command carries no borrow.
    pub uniforms: InlineUniforms,
    /// Scissor rect in physical pixels `(x, y, w, h)`, if the batch is clipped.
    pub scissor: Option<(u32, u32, u32, u32)>,
}

/// A flat, backend-neutral draw list for one frame: the packet `viso-render`
/// hands to [`GpuBackend::encode`].
pub struct DrawList<'a> {
    /// All draw commands for the frame, concatenated across passes in execution
    /// order. Each [`RenderPass`] in `passes` indexes a contiguous range here.
    pub commands: &'a [DrawCommand],
    /// The passes, in execution order (offscreen layers first, then main). Each
    /// references its commands by range into `commands`.
    pub passes: &'a [RenderPass],
}

/// The single RHI trait. Cold-path methods create resources; the hot path is
/// `write_buffer` + `encode` + `present`.
pub trait GpuBackend {
    /// Create a GPU buffer.
    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId;
    /// Create a texture.
    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId;
    /// Create a sampler.
    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId;

    /// Create a render pipeline. `layout` is the `#[derive(GpuInstance)]` layout
    /// of the instance type; it is validated against `desc.instance_schema`
    /// before the pipeline is built (registration-time layout check, §32).
    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError>;

    /// Create a bind group.
    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId;

    /// Overwrite a region of a buffer with CPU bytes (ring/persistent upload).
    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]);

    /// Overwrite a region of a texture (atlas dirty-rect upload).
    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]);

    /// Create a swapchain surface bound to a native window.
    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId;
    /// Resize a surface's swapchain.
    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32);

    /// Acquire the next drawable for `surface`.
    fn begin_frame(&mut self, surface: SurfaceId) -> Frame;
    /// Encode and submit a draw list.
    fn encode(&mut self, list: &DrawList<'_>);
    /// Present a previously begun frame.
    fn present(&mut self, frame: Frame);

    /// Static device capabilities.
    fn caps(&self) -> &Caps;

    /// The surface's swapchain format (color attachment format for pipelines).
    fn surface_format(&self, surface: SurfaceId) -> TextureFormat;
}
