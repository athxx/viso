//! Every built-in program in every backend language.
//!
//! [`shader_code`] hands a pipeline its program in the [`ShaderLang`] the
//! compiled backend consumes, so a build only ever materializes its own
//! language:
//!
//! - MSL and the two text dialects (WGSL, HLSL) are printed from the shader IR
//!   on first request and cached to `'static` — a cold, device-init cost;
//! - SPIR-V is frozen: the committed `spirv/<name>.spv` modules are generated
//!   from the native WGSL by the reference compiler and embedded as bytes, so no
//!   shader compiler ships in the binary. A test regenerates every module and
//!   requires byte equality (`VISO_BLESS_SPIRV=1` rewrites them).

use std::sync::OnceLock;

use viso_gpu::{ShaderCode, ShaderLang};

use crate::ir::codegen_hlsl::emit_hlsl;
use crate::ir::codegen_wgsl::{WgslTarget, emit_wgsl};
use crate::ir::module::{
    ShaderIr, advanced_blend_ir, analytic_capsule_ir, analytic_ellipse_ir, analytic_line_ir,
    analytic_rrect_ir, analytic_shadow_ir, blur_ir, color_transform_ir, glyphrun_ir, gradient_ir,
    image_ir, material_ir, mesh_ir, mtsdf_ir, quad_ir,
};
use crate::msl::{PrimitiveKind, shader_source};

/// One built-in program: its artifact name, IR constructor and frozen SPIR-V.
pub(crate) struct Program {
    /// The `spirv/<name>.spv` stem.
    #[cfg(test)]
    pub name: &'static str,
    /// The IR it is printed from.
    pub ir: fn() -> ShaderIr,
    /// The frozen SPIR-V module.
    pub spirv: &'static [u8],
}

macro_rules! programs {
    ($($name:literal => $ir:ident),* $(,)?) => {
        pub(crate) const PROGRAMS: &[Program] = &[$(Program {
            #[cfg(test)]
            name: $name,
            ir: $ir,
            spirv: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/spirv/", $name, ".spv")),
        }),*];
    };
}

programs! {
    "quad" => quad_ir,
    "image" => image_ir,
    "blur" => blur_ir,
    "color_transform" => color_transform_ir,
    "advanced_blend" => advanced_blend_ir,
    "material" => material_ir,
    "glyphrun" => glyphrun_ir,
    "mtsdf" => mtsdf_ir,
    "mesh" => mesh_ir,
    "analytic_rrect" => analytic_rrect_ir,
    "analytic_shadow" => analytic_shadow_ir,
    "analytic_ellipse" => analytic_ellipse_ir,
    "analytic_capsule" => analytic_capsule_ir,
    "analytic_line" => analytic_line_ir,
    "gradient" => gradient_ir,
}

/// The index into [`PROGRAMS`] of `kind`'s program, if it is shaded.
fn program_index(kind: PrimitiveKind) -> Option<usize> {
    Some(match kind {
        PrimitiveKind::Quad => 0,
        PrimitiveKind::Image => 1,
        PrimitiveKind::Blur => 2,
        PrimitiveKind::ColorTransform => 3,
        PrimitiveKind::AdvancedBlend => 4,
        PrimitiveKind::Material => 5,
        PrimitiveKind::GlyphRun => 6,
        PrimitiveKind::Mtsdf => 7,
        // Path and Mesh share the general per-vertex mesh pipeline.
        PrimitiveKind::Path | PrimitiveKind::Mesh => 8,
        PrimitiveKind::AnalyticRRect => 9,
        PrimitiveKind::AnalyticShadow => 10,
        PrimitiveKind::AnalyticEllipse => 11,
        PrimitiveKind::AnalyticCapsule => 12,
        PrimitiveKind::AnalyticLine => 13,
        PrimitiveKind::Gradient => 14,
        PrimitiveKind::Layer => return None,
    })
}

/// `kind`'s program in `lang`, or `None` if `kind` has no shader.
pub fn shader_code(kind: PrimitiveKind, lang: ShaderLang) -> Option<ShaderCode> {
    static WGSL: [OnceLock<String>; 15] = [const { OnceLock::new() }; 15];
    static HLSL: [OnceLock<String>; 15] = [const { OnceLock::new() }; 15];

    let i = program_index(kind)?;
    let program = &PROGRAMS[i];
    Some(match lang {
        ShaderLang::None => ShaderCode::None,
        ShaderLang::Msl => ShaderCode::Msl(shader_source(kind)?),
        ShaderLang::Wgsl => ShaderCode::Wgsl(WGSL[i].get_or_init(|| {
            emit_wgsl(&(program.ir)(), WgslTarget::Web)
                .expect("every built-in body passes the checker (tested)")
        })),
        ShaderLang::Hlsl => ShaderCode::Hlsl(HLSL[i].get_or_init(|| {
            emit_hlsl(&(program.ir)()).expect("every built-in body passes the checker (tested)")
        })),
        ShaderLang::SpirV => ShaderCode::SpirV(program.spirv),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::module::builtin_irs;
    use std::path::PathBuf;

    /// The SPIR-V the reference compiler makes from `ir`'s native WGSL.
    fn compile_spirv(ir: &ShaderIr) -> Vec<u8> {
        let src = emit_wgsl(ir, WgslTarget::Native).unwrap();
        let module = naga::front::wgsl::parse_str(&src).unwrap();
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::IMMEDIATES,
        )
        .validate(&module)
        .unwrap();
        let options = naga::back::spv::Options {
            lang_version: (1, 0),
            // Clip-space y is flipped in the vertex stage so the Vulkan viewport
            // stays the plain top-left one; no debug names ship.
            flags: naga::back::spv::WriterFlags::ADJUST_COORDINATE_SPACE,
            // Every loop in the bodies is bounded by construction.
            force_loop_bounding: false,
            ..Default::default()
        };
        let words = naga::back::spv::write_vec(&module, &info, &options, None).unwrap();
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn spirv_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("spirv")
    }

    #[test]
    fn programs_cover_every_builtin_in_order() {
        let irs = builtin_irs();
        assert_eq!(PROGRAMS.len(), irs.len());
        for (p, ir) in PROGRAMS.iter().zip(&irs) {
            assert_eq!((p.ir)().kind, ir.kind, "{}", p.name);
            assert_eq!(
                program_index(ir.kind).map(|i| PROGRAMS[i].name),
                Some(p.name)
            );
        }
    }

    #[test]
    fn frozen_spirv_matches_the_ir() {
        let bless = std::env::var_os("VISO_BLESS_SPIRV").is_some();
        for p in PROGRAMS {
            let fresh = compile_spirv(&(p.ir)());
            if bless {
                std::fs::create_dir_all(spirv_dir()).unwrap();
                std::fs::write(spirv_dir().join(format!("{}.spv", p.name)), &fresh).unwrap();
                continue;
            }
            assert!(
                fresh == p.spirv,
                "spirv/{}.spv is stale; regenerate with VISO_BLESS_SPIRV=1",
                p.name
            );
        }
    }

    #[test]
    fn frozen_spirv_passes_the_khronos_validator() {
        for p in PROGRAMS {
            assert_eq!(p.spirv.len() % 4, 0, "{}", p.name);
            assert_eq!(&p.spirv[..4], &0x0723_0203u32.to_le_bytes(), "{}", p.name);
            let path = spirv_dir().join(format!("{}.spv", p.name));
            let Ok(out) = std::process::Command::new("spirv-val")
                .args(["--target-env", "vulkan1.0"])
                .arg(&path)
                .output()
            else {
                eprintln!("spirv-val not installed; SPIR-V structure unchecked");
                return;
            };
            assert!(
                out.status.success(),
                "{}: {}",
                p.name,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn each_language_is_served_for_every_shaded_kind() {
        for lang in [
            ShaderLang::Msl,
            ShaderLang::Wgsl,
            ShaderLang::Hlsl,
            ShaderLang::SpirV,
        ] {
            let code = shader_code(PrimitiveKind::Blur, lang).unwrap();
            assert_eq!(code.lang(), lang);
        }
        assert_eq!(shader_code(PrimitiveKind::Layer, ShaderLang::Msl), None);
        assert_eq!(
            shader_code(PrimitiveKind::Path, ShaderLang::Hlsl),
            shader_code(PrimitiveKind::Mesh, ShaderLang::Hlsl)
        );
    }
}
