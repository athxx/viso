//! `__per_backend!` keeps the expression of the backend this target selects.

use viso_gpu::{Backend, GpuBackend, ShaderLang};

#[test]
fn the_per_backend_choice_is_the_target_backends_language() {
    let chosen = viso_gpu::__per_backend!(
        msl: ShaderLang::Msl,
        hlsl: ShaderLang::Hlsl,
        spirv: ShaderLang::SpirV,
        glsl: ShaderLang::Glsl,
        wgsl: ShaderLang::Wgsl,
        none: ShaderLang::None,
    );
    assert_eq!(chosen, <Backend as GpuBackend>::SHADER_LANG);
}
