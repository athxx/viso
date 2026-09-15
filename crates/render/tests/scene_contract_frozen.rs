//! F3 frozen-contract pin: the retained scene's identity, revision, and bounds
//! surface (§8, §8.4, §8.5). F4 — instance pool, upload ring, coalescer, batch
//! planner, render chunk — binds to exactly these shapes; D/C/E/M/A build on the
//! scene above them. A silent shift in any of them (a field widened, a plane
//! dropped, an id no longer mirroring the RHI handle) would ripple invisibly
//! into every consumer, so this test freezes them all in one place.
//!
//! The per-module unit tests already guard each shape in isolation; this
//! integration test is the single consolidated gate that fails loudly the moment
//! the *contract* — not just one module's internals — moves.

use std::mem::{align_of, size_of};

use viso_render::scene::bounds::Bounds;
use viso_render::scene::ids::{
    BrushId, ClipChainId, ClipId, EffectChainId, GeometryId, ImageId, MaterialId, MeshId,
    PaintChunkId, PathId, PrimitiveId, RenderChunkId, SceneId, TransformId,
};
use viso_render::scene::revision::Revisions;

/// One revision plane's `(bump fn, read fn)` pair — the table below drives every
/// plane through the same independence check.
type PlaneProbe = (fn(&mut Revisions), fn(&Revisions) -> u64);

/// The typed generational scene id is a frozen eight-byte `{index, generation}`
/// pair, four-byte aligned, mirroring `viso_gpu::slots::RawId` so one identity
/// discipline spans the whole stack (§8.2).
#[test]
fn scene_id_shape_is_frozen() {
    assert_eq!(size_of::<SceneId>(), 8, "SceneId is two u32s");
    assert_eq!(align_of::<SceneId>(), 4);
    assert_eq!(
        size_of::<SceneId>(),
        size_of::<viso_gpu::slots::RawId>(),
        "SceneId mirrors the RHI handle shape"
    );
    assert_eq!(align_of::<SceneId>(), align_of::<viso_gpu::slots::RawId>());

    // `new` is generation zero — the value a store hands out on first positional
    // assignment; `index`/`generation` read the two halves back.
    let id = SceneId::new(9);
    assert_eq!(id.index, 9);
    assert_eq!(id.generation, 0);
}

/// Every typed handle is a transparent newtype over `SceneId`: same eight bytes,
/// same alignment, with `new`/`index`/`generation`. The set of typed ids is
/// itself frozen — F4 chunk/material records name them by type.
#[test]
fn every_typed_id_is_a_transparent_scene_id() {
    macro_rules! assert_transparent {
        ($ty:ty) => {{
            assert_eq!(
                size_of::<$ty>(),
                8,
                concat!(stringify!($ty), " is a SceneId")
            );
            assert_eq!(align_of::<$ty>(), 4);
            let h = <$ty>::new(3);
            assert_eq!(h.index(), 3);
            assert_eq!(h.generation(), 0);
        }};
    }

    assert_transparent!(PrimitiveId);
    assert_transparent!(TransformId);
    assert_transparent!(BrushId);
    assert_transparent!(ClipId);
    assert_transparent!(ImageId);
    assert_transparent!(GeometryId);
    assert_transparent!(PathId);
    assert_transparent!(MeshId);
    assert_transparent!(ClipChainId);
    assert_transparent!(EffectChainId);
    assert_transparent!(MaterialId);
    assert_transparent!(RenderChunkId);
    assert_transparent!(PaintChunkId);
}

/// The scene tracks exactly seven independent revision planes, each a `u64` bump
/// counter (§8.4). The struct is 56 bytes of plain counters — a consumer
/// snapshots it and compares field-wise. A dropped or widened plane is a break.
#[test]
fn revision_planes_are_frozen() {
    assert_eq!(
        size_of::<Revisions>(),
        7 * size_of::<u64>(),
        "seven u64 planes, no padding"
    );
    assert_eq!(align_of::<Revisions>(), align_of::<u64>());
    assert_eq!(Revisions::default(), Revisions::new());

    // Each plane bumps alone: a change to one leaves the other six untouched,
    // the core §8.4 guarantee every consumer relies on.
    let base = Revisions::new();
    let bumps: [PlaneProbe; 7] = [
        (Revisions::bump_geometry, |r| r.geometry),
        (Revisions::bump_paint, |r| r.paint),
        (Revisions::bump_transform, |r| r.transform),
        (Revisions::bump_clip, |r| r.clip),
        (Revisions::bump_resource, |r| r.resource),
        (Revisions::bump_effect, |r| r.effect),
        (Revisions::bump_visibility, |r| r.visibility),
    ];
    let total = |r: &Revisions| {
        r.geometry + r.paint + r.transform + r.clip + r.resource + r.effect + r.visibility
    };
    for (bump, read) in bumps {
        let mut r = base;
        bump(&mut r);
        assert_eq!(read(&r), 1, "the bumped plane advanced by one");
        assert_eq!(total(&r), 1, "no other plane moved");
    }
}

/// A primitive carries exactly the five-stage bounds set
/// `local`/`world`/`clip`/`paint`/`effect` (§8), and `paint` inflates by half
/// the stroke width plus the filter footprint — computed numerically, never by
/// re-parsing a path. F4's dirty coalescer and visibility read `paint`.
#[test]
fn bounds_set_is_frozen() {
    use viso_render::Rect;

    // Default is all-zero: five zero rects.
    let d = Bounds::default();
    for r in [d.local, d.world, d.clip, d.paint, d.effect] {
        assert_eq!(r, Rect::ZERO);
    }

    // `paint = world.inflate(stroke * 0.5 + filter)`; the other stages resolve
    // from the same already-resolved world extent.
    let world = Rect {
        x: 10.0,
        y: 10.0,
        w: 20.0,
        h: 20.0,
    };
    let b = Bounds::from_world(world, None, 4.0, 1.0);
    assert_eq!(b.local, world);
    assert_eq!(b.world, world);
    assert_eq!(b.clip, world, "no clip narrows nothing");
    // 2px (half of 4px stroke) + 1px filter = 3px on every side.
    assert_eq!(
        b.paint,
        Rect {
            x: 7.0,
            y: 7.0,
            w: 26.0,
            h: 26.0,
        }
    );
    assert_eq!(b.effect, b.paint, "no neighbourhood effect grows paint");
}
