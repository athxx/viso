//! Frozen `#[repr(C)]` layout of the GPU instance/vertex structs (§18 / §36.1).
//!
//! These structs are the CPU half of the three-way instance ABI: the
//! `#[derive(GpuPod)]` layout here, the shader's `InstanceIn`/`VertexIn` struct,
//! and the headless field reader must all agree. F3 (retained stores) and F4
//! (instance pool / upload ring / coalescer) place these bytes directly into
//! long-lived device buffers and diff them by slot, so any shift in a field
//! offset or the stride silently corrupts every consumer downstream.
//!
//! `dev_shader_pipeline.rs` and `msl.rs` already pin the field *order* against
//! the shader schema. This test pins the *bytes*: `size_of`, `align_of`, and the
//! offset of every field. A change to any of these is a deliberate ABI break —
//! update the shader schema, the headless reader, and this snapshot together, and
//! re-freeze.

use std::mem::{align_of, offset_of, size_of};

use viso_render::{
    AnalyticCapsuleInstance, AnalyticEllipseInstance, AnalyticLineInstance, AnalyticRRectInstance,
    GlyphInstance, GradientInstance, ImageInstance, MeshVertex, QuadInstance,
};

#[test]
fn quad_instance_layout_is_frozen() {
    assert_eq!(size_of::<QuadInstance>(), 56, "QuadInstance stride");
    assert_eq!(align_of::<QuadInstance>(), 4, "QuadInstance align");
    assert_eq!(offset_of!(QuadInstance, rect_pos), 0);
    assert_eq!(offset_of!(QuadInstance, rect_size), 8);
    assert_eq!(offset_of!(QuadInstance, color), 16);
    assert_eq!(offset_of!(QuadInstance, radius), 32);
    assert_eq!(offset_of!(QuadInstance, border_width), 36);
    assert_eq!(offset_of!(QuadInstance, border_color), 40);
}

#[test]
fn analytic_rrect_instance_layout_is_frozen() {
    assert_eq!(
        size_of::<AnalyticRRectInstance>(),
        68,
        "AnalyticRRectInstance stride"
    );
    assert_eq!(
        align_of::<AnalyticRRectInstance>(),
        4,
        "AnalyticRRectInstance align"
    );
    assert_eq!(offset_of!(AnalyticRRectInstance, rect_pos), 0);
    assert_eq!(offset_of!(AnalyticRRectInstance, rect_size), 8);
    assert_eq!(offset_of!(AnalyticRRectInstance, color), 16);
    assert_eq!(offset_of!(AnalyticRRectInstance, radius), 32);
    assert_eq!(offset_of!(AnalyticRRectInstance, border_width), 48);
    assert_eq!(offset_of!(AnalyticRRectInstance, border_color), 52);
}

#[test]
fn analytic_ellipse_instance_layout_is_frozen() {
    assert_eq!(
        size_of::<AnalyticEllipseInstance>(),
        52,
        "AnalyticEllipseInstance stride"
    );
    assert_eq!(
        align_of::<AnalyticEllipseInstance>(),
        4,
        "AnalyticEllipseInstance align"
    );
    assert_eq!(offset_of!(AnalyticEllipseInstance, rect_pos), 0);
    assert_eq!(offset_of!(AnalyticEllipseInstance, rect_size), 8);
    assert_eq!(offset_of!(AnalyticEllipseInstance, color), 16);
    assert_eq!(offset_of!(AnalyticEllipseInstance, border_width), 32);
    assert_eq!(offset_of!(AnalyticEllipseInstance, border_color), 36);
}

#[test]
fn analytic_capsule_instance_layout_is_frozen() {
    assert_eq!(
        size_of::<AnalyticCapsuleInstance>(),
        52,
        "AnalyticCapsuleInstance stride"
    );
    assert_eq!(
        align_of::<AnalyticCapsuleInstance>(),
        4,
        "AnalyticCapsuleInstance align"
    );
    assert_eq!(offset_of!(AnalyticCapsuleInstance, rect_pos), 0);
    assert_eq!(offset_of!(AnalyticCapsuleInstance, rect_size), 8);
    assert_eq!(offset_of!(AnalyticCapsuleInstance, color), 16);
    assert_eq!(offset_of!(AnalyticCapsuleInstance, border_width), 32);
    assert_eq!(offset_of!(AnalyticCapsuleInstance, border_color), 36);
}

#[test]
fn analytic_line_instance_layout_is_frozen() {
    assert_eq!(
        size_of::<AnalyticLineInstance>(),
        68,
        "AnalyticLineInstance stride"
    );
    assert_eq!(
        align_of::<AnalyticLineInstance>(),
        4,
        "AnalyticLineInstance align"
    );
    assert_eq!(offset_of!(AnalyticLineInstance, p0), 0);
    assert_eq!(offset_of!(AnalyticLineInstance, p1), 8);
    assert_eq!(offset_of!(AnalyticLineInstance, width), 16);
    assert_eq!(offset_of!(AnalyticLineInstance, color), 20);
    assert_eq!(offset_of!(AnalyticLineInstance, cap), 36);
    assert_eq!(offset_of!(AnalyticLineInstance, join), 40);
    assert_eq!(offset_of!(AnalyticLineInstance, miter_limit), 44);
    assert_eq!(offset_of!(AnalyticLineInstance, border_width), 48);
    assert_eq!(offset_of!(AnalyticLineInstance, border_color), 52);
}

#[test]
fn image_instance_layout_is_frozen() {
    assert_eq!(size_of::<ImageInstance>(), 48, "ImageInstance stride");
    assert_eq!(align_of::<ImageInstance>(), 4, "ImageInstance align");
    assert_eq!(offset_of!(ImageInstance, rect_pos), 0);
    assert_eq!(offset_of!(ImageInstance, rect_size), 8);
    assert_eq!(offset_of!(ImageInstance, uv_pos), 16);
    assert_eq!(offset_of!(ImageInstance, uv_size), 24);
    assert_eq!(offset_of!(ImageInstance, color), 32);
}

#[test]
fn glyph_instance_layout_is_frozen() {
    // Structurally identical to ImageInstance, but frozen independently: the two
    // are separate types bound to separate shader families, and must be free to
    // diverge only by a deliberate, re-frozen ABI change.
    assert_eq!(size_of::<GlyphInstance>(), 48, "GlyphInstance stride");
    assert_eq!(align_of::<GlyphInstance>(), 4, "GlyphInstance align");
    assert_eq!(offset_of!(GlyphInstance, rect_pos), 0);
    assert_eq!(offset_of!(GlyphInstance, rect_size), 8);
    assert_eq!(offset_of!(GlyphInstance, uv_pos), 16);
    assert_eq!(offset_of!(GlyphInstance, uv_size), 24);
    assert_eq!(offset_of!(GlyphInstance, color), 32);
}

#[test]
fn gradient_instance_layout_is_frozen() {
    assert_eq!(size_of::<GradientInstance>(), 80, "GradientInstance stride");
    assert_eq!(align_of::<GradientInstance>(), 4, "GradientInstance align");
    assert_eq!(offset_of!(GradientInstance, rect_pos), 0);
    assert_eq!(offset_of!(GradientInstance, rect_size), 8);
    assert_eq!(offset_of!(GradientInstance, kind), 16);
    assert_eq!(offset_of!(GradientInstance, extend), 20);
    assert_eq!(offset_of!(GradientInstance, p0), 24);
    assert_eq!(offset_of!(GradientInstance, p1), 32);
    assert_eq!(offset_of!(GradientInstance, lut_v), 40);
    assert_eq!(offset_of!(GradientInstance, use_lut), 44);
    assert_eq!(offset_of!(GradientInstance, color0), 48);
    assert_eq!(offset_of!(GradientInstance, color1), 64);
}

#[test]
fn mesh_vertex_layout_is_frozen() {
    assert_eq!(size_of::<MeshVertex>(), 28, "MeshVertex stride");
    assert_eq!(align_of::<MeshVertex>(), 4, "MeshVertex align");
    assert_eq!(offset_of!(MeshVertex, pos), 0);
    assert_eq!(offset_of!(MeshVertex, color), 8);
    assert_eq!(offset_of!(MeshVertex, edge), 24);
}
