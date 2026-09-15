//! The dev-mode `ShaderPipeline` keep-last-good source cannot drift from the
//! frozen release manifest (architecture section 7.1 / §60).
//!
//! Release builds create every standard pipeline from the frozen
//! `standard_manifest()` MSL — compiled once at device init, never on a draw. In
//! development, a shader edit recompiles its IR through a `ShaderPipeline`, which
//! keeps the last-good compile so a broken edit never takes down the live
//! pipeline. Those are two producers of the same artifact, and §60 requires the
//! dev path to impose no divergence on release: the dev-mode compile of a
//! built-in's canonical IR must be *byte-identical* to that family's frozen
//! manifest MSL, and must validate against the very CPU instance layout the
//! renderer registers.
//!
//! This test is that tie. For each built-in it builds a `ShaderPipeline` from the
//! canonical IR and the real `#[derive(GpuPod)]` CPU layout — which runs the
//! §36.1 ABI validation — then asserts the pipeline's last-good MSL equals the
//! frozen manifest entry for the same family. A drift in either the IR→MSL
//! codegen or the manifest would break byte-equality here; an ABI mismatch
//! between the IR and the instance struct would fail construction.

use viso_gpu::InstanceLayout;
use viso_render::{GlyphInstance, ImageInstance, MeshVertex, QuadInstance};
use viso_shader::ir::module::{ShaderIr, glyphrun_ir, image_ir, mesh_ir, quad_ir};
use viso_shader::{PipelineFamily, ShaderPipeline, standard_manifest};

/// Build the dev-mode pipeline for one built-in and assert its keep-last-good MSL
/// is byte-identical to the frozen manifest entry for `family`.
fn assert_dev_matches_manifest(ir: ShaderIr, cpu_layout: &InstanceLayout, family: PipelineFamily) {
    // Constructing the pipeline runs the §36.1 ABI validation of the IR's
    // declared schema against the CPU instance layout the renderer registers; a
    // mismatch (shifted field, wrong format, bad stride) fails here.
    let pipeline = ShaderPipeline::new(&ir, cpu_layout)
        .expect("the canonical IR validates against its own instance layout");

    let entry = standard_manifest()
        .entry(family)
        .expect("the manifest populates every implemented built-in family");

    assert_eq!(
        pipeline.last_good().msl,
        entry.msl,
        "the dev-mode {family:?} compile must be byte-identical to the frozen \
         manifest MSL — the dev keep-last-good path may not diverge from the \
         release artifact (§60)"
    );
}

#[test]
fn dev_mode_pipelines_match_the_frozen_manifest() {
    assert_dev_matches_manifest(quad_ir(), &QuadInstance::LAYOUT, PipelineFamily::SolidRect);
    assert_dev_matches_manifest(image_ir(), &ImageInstance::LAYOUT, PipelineFamily::Image);
    assert_dev_matches_manifest(
        glyphrun_ir(),
        &GlyphInstance::LAYOUT,
        PipelineFamily::MaskComposite,
    );
    assert_dev_matches_manifest(mesh_ir(), &MeshVertex::LAYOUT, PipelineFamily::PathFill);
}
