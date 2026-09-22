//! A0's whole claim in one place (§20, §7.2): these are specializations, not
//! prerequisites.
//!
//! Three lanes were added — a compute tessellation lane, a bindless binding model, a
//! GPU cull feeding indirect draws — and each is pinned in its own file against its
//! own conditions. What that per-lane pinning cannot state is the property the three
//! share, because it is a property of the *set*: no ordinary frame enters any of
//! them, none of them is on unless something measured asked for it, and a renderer
//! whose backend offers none of the three still draws every D-section primitive
//! correctly.
//!
//! That last part is why this file is short. "D0~D3 remains correct with zero compute
//! dependency" is not one assertion; it is the entire rest of the suite passing on a
//! backend that reports every A0 capability absent — which is the only backend there
//! is here. What is left to state explicitly is the *defaults*, and that the three
//! capability answers really are all no.

use viso_gpu::{GpuBackend, HeadlessRaster};
use viso_render::{
    BindingModel, Border, CullPlan, CullWorkload, Primitive, Quad, Rect, Renderer, Rgba,
    TextureWorkload, VectorLane, VectorWorkload,
};

/// An undescribed workload selects the portable realization in all three lanes.
///
/// The shared shape of every A0 decision: the measured inputs default to zero, a
/// zeroed workload fails every condition, and so "nobody measured anything" and "stay
/// on the path that always works" are the same state (§7.3). A lane that defaulted to
/// its fast path would be a lane you had to opt *out* of.
#[test]
fn nothing_measured_means_nothing_specialized() {
    assert_eq!(
        VectorLane::select(VectorWorkload::default()),
        VectorLane::CpuTessellate
    );
    assert_eq!(
        BindingModel::select(TextureWorkload::default()),
        BindingModel::PerDraw
    );
    assert_eq!(
        CullPlan::select(CullWorkload::default()),
        CullPlan::PerPrimitive
    );

    assert_eq!(VectorLane::default(), VectorLane::CpuTessellate);
    assert_eq!(BindingModel::default(), BindingModel::PerDraw);
    assert_eq!(CullPlan::default(), CullPlan::PerPrimitive);
}

/// Every A0 capability is absent here, and each is reported as the thing it is rather
/// than as a boolean convenience: a dispatch entry point exists or does not, a
/// resource table has a size, an indirect draw reads its counts from memory or does
/// not. All three answers are no, so all three lanes are unreachable and the frames
/// below are the only frames this renderer can produce.
#[test]
fn this_backend_offers_none_of_the_three() {
    let gpu = HeadlessRaster::new();
    let caps = gpu.caps();
    assert!(!caps.compute_dispatch);
    assert_eq!(caps.bindless_texture_slots, 0);
    assert!(!caps.indirect_draw);
}

/// An ordinary frame, drawn for real, reports zero of every A0 counter — the
/// arithmetic form of "a specialization you did not ask for did not happen". A future
/// lane that leaked into the default path trips this without anyone reading a diff.
#[test]
fn an_ordinary_frame_reports_no_specialization() {
    const W: u32 = 256;
    const H: u32 = 256;

    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(viso_gpu::RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([W as f32, H as f32]);

    let scene: Vec<Primitive> = (0..64)
        .map(|i| {
            Primitive::Quad(Quad {
                rect: Rect {
                    x: 4.0 + (i % 8) as f32 * 31.0,
                    y: 4.0 + (i / 8) as f32 * 31.0,
                    w: 26.0,
                    h: 26.0,
                },
                color: Rgba::new(0.4, 0.4, 0.8, 1.0),
                radius: 6.0,
                border: Border::NONE,
            })
        })
        .collect();

    r.upload(&mut gpu, &scene);
    r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);

    let s = r.frame_stats();
    assert_eq!(s.compute_dispatches, 0, "no compute lane was entered");
    assert_eq!(s.indirect_draws, 0, "no draw read its counts from a buffer");
    assert!(s.draw_calls > 0, "and the frame was really drawn");
}
