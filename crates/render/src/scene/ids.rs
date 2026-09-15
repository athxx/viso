//! Typed dense generational identifiers for the retained scene (§8).
//!
//! Every retained entity — a primitive, a transform, a brush, a clip, an image,
//! a geometry, a path, a mesh, a chunk — is addressed by a fixed-width
//! `{index, generation}` handle rather than a pointer or a bare `usize`. The
//! pair is the same shape the GPU RHI uses for resource handles
//! (`viso_gpu::slots::RawId`), which keeps one identity discipline across the
//! whole stack: `index` selects a dense storage slot, and `generation` is the
//! guard that makes a handle left over from a reclaimed slot resolve to nothing
//! instead of silently aliasing whatever later took its place.
//!
//! The scene handles differ from the GPU ones in *how the index is assigned*.
//! GPU resources are created and destroyed explicitly, so their slots are
//! free-listed. Scene entities are **positionally assigned**: the Nth primitive
//! of a kind in the flat primitive stream owns the Nth slot of its store, frame
//! after frame, because the sole producer re-emits the whole tree in stable
//! pre-order every frame (`ui::component::repaint_dirty`). Positional identity
//! is what lets the ingest diff (F3.2) recognise an unchanged primitive and
//! mutate nothing. The `generation` field still travels in the handle so a slot
//! reused after a structural change (a kind swap, a shrink) invalidates stale
//! handles held by tooling or chunk records; the steady-state diff path never
//! bumps it.

/// One typed scene handle: which dense slot, and which generation of that slot.
///
/// Layout is frozen: `#[repr(C)]`, two `u32`s, eight bytes, four-byte aligned —
/// identical to `viso_gpu::slots::RawId` so the two never drift, and so a
/// handle can be stored in a packed side table or a chunk record without
/// padding surprises. Every typed id below is a thin newtype over this pair.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneId {
    /// Dense storage-slot index within the owning store.
    pub index: u32,
    /// Slot generation at the time the handle was issued; bumped when the slot
    /// is reslotted on the cold structural path so stale handles miss.
    pub generation: u32,
}

impl SceneId {
    /// The handle for `index` in generation zero — the value a store hands out
    /// the first time a slot is positionally assigned.
    #[inline]
    pub const fn new(index: u32) -> SceneId {
        SceneId {
            index,
            generation: 0,
        }
    }
}

/// Emit a typed newtype wrapping [`SceneId`], so each store stamps its own type
/// on the handle and a `PrimitiveId` can never be passed where a `TransformId`
/// is expected. All newtypes share the frozen eight-byte `{index, generation}`
/// layout; the wrapper is `#[repr(transparent)]` so it is exactly a `SceneId`.
macro_rules! scene_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub SceneId);

        impl $name {
            /// The handle for `index` in generation zero.
            #[inline]
            pub const fn new(index: u32) -> $name {
                $name(SceneId::new(index))
            }

            /// The dense slot index this handle addresses.
            #[inline]
            pub const fn index(self) -> u32 {
                self.0.index
            }

            /// The generation this handle was issued at.
            #[inline]
            pub const fn generation(self) -> u32 {
                self.0.generation
            }
        }
    };
}

scene_id!(
    /// A primitive's stable identity across frames, assigned positionally by the
    /// ingest walk (§8). The retained analogue of a node's paint slot.
    PrimitiveId
);
scene_id!(
    /// A transform entry, separated from its primitive so a pure move dirties the
    /// transform plane alone (§8, §8.5).
    TransformId
);
scene_id!(
    /// A brush (fill/stroke paint) entry, separated so a recolor dirties the
    /// paint plane alone (§8.5).
    BrushId
);
scene_id!(
    /// A clip-rect entry in the clip store (§8).
    ClipId
);
scene_id!(
    /// An image draw's entry in the image store (§8).
    ImageId
);
scene_id!(
    /// A geometry entry — the resolved shape of a primitive, independent of its
    /// paint and transform (§8).
    GeometryId
);
scene_id!(
    /// A vector path's entry in the path store; keys its tessellation cache (§8).
    PathId
);
scene_id!(
    /// A caller-supplied triangle mesh's entry in the mesh store (§8).
    MeshId
);
scene_id!(
    /// A resolved chain of nested clips (§8). Populated by F4's chunking.
    ClipChainId
);
scene_id!(
    /// A resolved chain of layer/filter effects (§8). Populated by F4.
    EffectChainId
);
scene_id!(
    /// A material (pipeline family + variant + resources) summary (§8).
    MaterialId
);
scene_id!(
    /// A render chunk — an order range with a uniform batch key (§8, §9). F4.
    RenderChunkId
);
scene_id!(
    /// A paint chunk — a coalesced run of same-paint primitives (§8). F4.
    PaintChunkId
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn scene_id_layout_is_frozen() {
        assert_eq!(size_of::<SceneId>(), 8, "SceneId is two u32s");
        assert_eq!(align_of::<SceneId>(), 4);
        // The typed newtypes are transparent wrappers: same size and align.
        assert_eq!(size_of::<PrimitiveId>(), 8);
        assert_eq!(align_of::<PrimitiveId>(), 4);
        assert_eq!(size_of::<RenderChunkId>(), 8);
    }

    #[test]
    fn scene_id_matches_gpu_raw_id_shape() {
        // The scene handles deliberately mirror the RHI handle shape so one
        // identity discipline spans the stack. A drift here is a break.
        assert_eq!(
            size_of::<SceneId>(),
            size_of::<viso_gpu::slots::RawId>(),
            "SceneId mirrors RawId"
        );
        assert_eq!(align_of::<SceneId>(), align_of::<viso_gpu::slots::RawId>());
    }

    #[test]
    fn new_is_generation_zero() {
        let id = PrimitiveId::new(7);
        assert_eq!(id.index(), 7);
        assert_eq!(id.generation(), 0);
    }
}
