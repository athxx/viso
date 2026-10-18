//! Typed GPU resource handles.

/// Typed, cheap-to-copy handles for GPU resources. Backends map these to
/// their own native objects; users and upper layers never see raw pointers.
///
/// Each handle is generation-safe: `index` selects a storage slot and
/// `generation` is bumped when that slot is reclaimed, so a handle left over
/// from a destroyed resource is detectable and never resolves to whatever new
/// resource later took its slot. The `{index, generation}` shape matches the
/// runtime `NodeId` (§8.2).
macro_rules! resource_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[repr(C)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name {
            /// Storage-slot index.
            pub index: u32,
            /// Slot generation; bumped on reclaim so stale handles miss.
            pub generation: u32,
        }

        impl $name {
            /// A handle to `index` in generation 0.
            #[inline]
            pub const fn new(index: u32) -> Self {
                Self {
                    index,
                    generation: 0,
                }
            }

            /// The storage-slot index this handle selects.
            #[inline]
            pub const fn index(self) -> u32 {
                self.index
            }
        }
    };
}

resource_id!(/// Handle to a GPU buffer.
    BufferId);
resource_id!(/// Handle to a GPU texture.
    TextureId);
resource_id!(/// Handle to a sampler.
    SamplerId);
resource_id!(/// Handle to a render/compute pipeline.
    PipelineId);
resource_id!(/// Handle to a bind group.
    BindGroupId);
resource_id!(/// Handle to a swapchain/surface.
    SurfaceId);
