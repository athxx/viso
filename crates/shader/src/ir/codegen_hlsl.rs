//! IR → HLSL (shader model 5.1) codegen, for Direct3D 12.
//!
//! [`emit_hlsl`] prints the interface of one [`ShaderIr`] from its typed field
//! lists and lowers the checked body tree with the HLSL [`Printer`]:
//!
//! - attribute fields carry the semantic `ATTR<i>` in declaration order — the
//!   D3D12 input layout names the same semantics, per-instance or per-vertex as
//!   the [`VertexSource`](crate::ir::module::VertexSource) says, so the body's
//!   fetch is the entry's `viso_attrs` parameter;
//! - `VOut` puts `SV_Position` on the position varying and `ATTR<i>` on the
//!   rest, `nointerpolation` for integer and `[[flat]]` varyings;
//! - the uniform block is `ConstantBuffer<Uniforms>` at `b0`, which the root
//!   signature maps to root constants;
//! - `tex`/`dst_tex` sit at `t0`/`t1` and the shared sampler at `s0`.
//!
//! `get_width()`/`get_height()` lower to two small prelude functions over
//! `GetDimensions`, emitted only for textured programs.

use std::fmt::Write as _;

use crate::ir::body::print::{ATTRS_PARAM, Lang, Printer};
use crate::ir::body::{BodyError, parse_ir};
use crate::ir::module::ShaderIr;
use crate::ir::types::{IrType, ScalarType};

/// The vertex-stage profile the D3D12 backend compiles [`emit_hlsl`] output with.
pub const VERTEX_PROFILE: &str = "vs_5_1";
/// The pixel-stage profile the D3D12 backend compiles [`emit_hlsl`] output with.
pub const PIXEL_PROFILE: &str = "ps_5_1";
/// The input-layout semantic name every attribute uses, indexed by position.
pub const ATTR_SEMANTIC: &str = "ATTR";

/// Emit the complete HLSL program for `ir`.
pub fn emit_hlsl(ir: &ShaderIr) -> Result<String, BodyError> {
    let parsed = parse_ir(ir)?;
    let names = Printer::new(Lang::Hlsl);
    let mut out = String::with_capacity(4096);

    let _ = writeln!(out, "struct {} {{", ir.vertex_source.attr_struct_name());
    for (i, f) in ir.attributes.iter().enumerate() {
        let _ = writeln!(
            out,
            "    {} {} : {ATTR_SEMANTIC}{i};",
            hlsl_type(f.ty),
            names.ident(f.name)
        );
    }
    out.push_str("};\n\n");

    out.push_str("struct Uniforms {\n");
    for f in ir.uniforms {
        let _ = writeln!(out, "    {} {};", hlsl_type(f.ty), names.ident(f.name));
    }
    out.push_str("};\n\n");

    out.push_str("struct VOut {\n");
    let mut location = 0;
    for v in ir.varyings {
        let name = names.ident(v.name);
        let ty = hlsl_type(v.ty);
        if v.attr.contains("[[position]]") {
            let _ = writeln!(out, "    {ty} {name} : SV_Position;");
            continue;
        }
        let flat = v.attr.contains("[[flat]]") || !is_float(v.ty);
        let interp = if flat { "nointerpolation " } else { "" };
        let _ = writeln!(out, "    {interp}{ty} {name} : {ATTR_SEMANTIC}{location};");
        location += 1;
    }
    out.push_str("};\n\n");

    out.push_str("ConstantBuffer<Uniforms> uniforms : register(b0);\n");
    if ir.texture_count >= 1 {
        out.push_str("Texture2D<float4> tex : register(t0);\n");
        if ir.texture_count >= 2 {
            out.push_str("Texture2D<float4> dst_tex : register(t1);\n");
        }
        out.push_str("SamplerState samp : register(s0);\n\n");
        out.push_str(
            "uint viso_width(Texture2D<float4> t) {\n    uint w;\n    uint h;\n    \
             t.GetDimensions(w, h);\n    return w;\n}\n\n",
        );
        out.push_str(
            "uint viso_height(Texture2D<float4> t) {\n    uint w;\n    uint h;\n    \
             t.GetDimensions(w, h);\n    return h;\n}\n",
        );
    }
    out.push('\n');

    let _ = writeln!(
        out,
        "VOut vertex_main({} {ATTRS_PARAM}, uint vid : SV_VertexID, \
         uint iid : SV_InstanceID) {{",
        ir.vertex_source.attr_struct_name()
    );
    let mut p = Printer::new(Lang::Hlsl).with_depth(1);
    p.entry_body(&parsed.vertex);
    out.push_str(&p.finish());
    out.push_str("}\n");

    if !parsed.helpers.is_empty() {
        out.push('\n');
        let mut p = Printer::new(Lang::Hlsl);
        p.items(&parsed.helpers);
        out.push_str(&p.finish());
    }

    out.push_str("\nfloat4 fragment_main(VOut vin) : SV_Target {\n");
    let mut p = Printer::new(Lang::Hlsl).with_depth(1);
    p.entry_body(&parsed.fragment);
    out.push_str(&p.finish());
    out.push_str("}\n");
    Ok(out)
}

/// The HLSL spelling of an interface type.
fn hlsl_type(t: IrType) -> String {
    let scalar = |s: ScalarType| match s {
        ScalarType::F32 => "float",
        ScalarType::U32 => "uint",
    };
    match t {
        IrType::Scalar(s) => scalar(s).to_string(),
        IrType::Vector { scalar: s, lanes } => format!("{}{lanes}", scalar(s)),
    }
}

fn is_float(t: IrType) -> bool {
    matches!(
        t,
        IrType::Scalar(ScalarType::F32)
            | IrType::Vector {
                scalar: ScalarType::F32,
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::module::builtin_irs;
    use std::path::PathBuf;
    use std::process::Command;

    /// Compile one stage with glslang's HLSL front end and validate the SPIR-V.
    /// Returns `None` when the tools are not installed.
    fn glslang(src: &str, stage: &str, entry: &str, tag: &str) -> Option<Result<(), String>> {
        let dir: PathBuf = std::env::temp_dir().join("viso_hlsl_check");
        std::fs::create_dir_all(&dir).ok()?;
        let hlsl = dir.join(format!("{tag}.hlsl"));
        let spv = dir.join(format!("{tag}.{stage}.spv"));
        std::fs::write(&hlsl, src).ok()?;
        let out = Command::new("glslangValidator")
            .args([
                "-D",
                "-S",
                stage,
                "-e",
                entry,
                "--target-env",
                "vulkan1.1",
                "-o",
            ])
            .arg(&spv)
            .arg(&hlsl)
            .output()
            .ok()?;
        if !out.status.success() {
            return Some(Err(String::from_utf8_lossy(&out.stdout).into_owned()));
        }
        let val = Command::new("spirv-val").arg(&spv).output().ok()?;
        if !val.status.success() {
            return Some(Err(String::from_utf8_lossy(&val.stderr).into_owned()));
        }
        Some(Ok(()))
    }

    #[test]
    fn every_builtin_compiles_as_hlsl() {
        for ir in builtin_irs() {
            let src = emit_hlsl(&ir).unwrap();
            let tag = format!("{:?}", ir.kind);
            for (stage, entry) in [("vert", "vertex_main"), ("frag", "fragment_main")] {
                match glslang(&src, stage, entry, &tag) {
                    None => {
                        eprintln!("glslangValidator/spirv-val not installed; HLSL unchecked");
                        return;
                    }
                    Some(Err(e)) => panic!("{tag} {stage}:\n{e}\n{src}"),
                    Some(Ok(())) => {}
                }
            }
        }
    }

    #[test]
    fn semantics_follow_declaration_order() {
        let ir = builtin_irs()[0].clone();
        let src = emit_hlsl(&ir).unwrap();
        for (i, f) in ir.attributes.iter().enumerate() {
            assert!(src.contains(&format!(" {} : ATTR{i};", f.name)), "{src}");
        }
        assert!(src.contains(": SV_Position;"));
        assert!(src.contains("ConstantBuffer<Uniforms> uniforms : register(b0);"));
    }
}
