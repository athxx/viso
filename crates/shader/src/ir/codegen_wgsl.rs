//! IR → WGSL codegen, for WebGPU and (through SPIR-V) Vulkan.
//!
//! [`emit_wgsl`] prints the interface of one [`ShaderIr`] from its typed field
//! lists and lowers the checked body tree with the WGSL [`Printer`]:
//!
//! - the attribute struct carries one `@location(i)` per field, in declaration
//!   order — the pipeline's vertex-buffer layout feeds it with the step mode the
//!   [`VertexSource`] names, so the body's `instances[iid]` / `verts[vid]` fetch
//!   is the entry's `viso_attrs` parameter;
//! - `VOut` numbers its varyings after `@builtin(position)`; integer and
//!   `[[flat]]` varyings are `@interpolate(flat)`;
//! - the uniform block is a `var<uniform>` at `@group(0) @binding(0)` on
//!   [`WgslTarget::Web`], and a `var<immediate>` (push constants) on
//!   [`WgslTarget::Native`], the form the SPIR-V artifacts are made from;
//! - textures and the shared sampler sit in `@group(1)` on the web and
//!   `@group(0)` natively (the only descriptor set): `tex` at binding 0,
//!   `dst_tex` at 1, `samp` at 2.
//!
//! The module opens with `diagnostic(off, derivative_uniformity)`: the analytic
//! bodies take screen-space derivatives in helpers called under per-fragment
//! branches exactly as the MSL does, and those branches are uniform within the
//! quads that matter (a primitive's own coverage), so the conservative analysis
//! is switched off rather than rewriting the algorithms. Texture reads are
//! explicit-LOD and need no uniformity.

use std::fmt::Write as _;

use crate::ir::body::print::{ATTRS_PARAM, Lang, Printer};
use crate::ir::body::{BodyError, parse_ir};
use crate::ir::module::{IrField, ShaderIr, Varying};
use crate::ir::types::{IrType, ScalarType};

/// Which WGSL consumer the module is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WgslTarget {
    /// A WebGPU device: uniforms in a bound uniform buffer.
    Web,
    /// SPIR-V generation for Vulkan: uniforms as push constants.
    Native,
}

/// Emit the complete WGSL module for `ir`.
pub fn emit_wgsl(ir: &ShaderIr, target: WgslTarget) -> Result<String, BodyError> {
    let parsed = parse_ir(ir)?;
    let names = Printer::new(Lang::Wgsl);
    let mut out = String::with_capacity(4096);
    out.push_str("diagnostic(off, derivative_uniformity);\n\n");

    let _ = writeln!(out, "struct {} {{", ir.vertex_source.attr_struct_name());
    for (i, f) in ir.attributes.iter().enumerate() {
        let _ = writeln!(
            out,
            "    @location({i}) {}: {},",
            names.ident(f.name),
            wgsl_type(f.ty)
        );
    }
    out.push_str("}\n\n");

    out.push_str("struct Uniforms {\n");
    for f in ir.uniforms {
        push_field(&mut out, &names, f);
    }
    out.push_str("}\n\n");

    out.push_str("struct VOut {\n");
    let mut location = 0;
    for v in ir.varyings {
        push_varying(&mut out, &names, v, &mut location);
    }
    out.push_str("}\n\n");

    match target {
        WgslTarget::Web => {
            out.push_str("@group(0) @binding(0) var<uniform> uniforms: Uniforms;\n");
        }
        WgslTarget::Native => out.push_str("var<immediate> uniforms: Uniforms;\n"),
    }
    // With the uniforms in push constants, the textures take the only set.
    let group = match target {
        WgslTarget::Web => 1,
        WgslTarget::Native => 0,
    };
    if ir.texture_count >= 1 {
        let _ = writeln!(out, "@group({group}) @binding(0) var tex: texture_2d<f32>;");
        if ir.texture_count >= 2 {
            let _ = writeln!(
                out,
                "@group({group}) @binding(1) var dst_tex: texture_2d<f32>;"
            );
        }
        let _ = writeln!(out, "@group({group}) @binding(2) var samp: sampler;");
    }
    out.push('\n');

    let _ = writeln!(
        out,
        "@vertex\nfn vertex_main(@builtin(vertex_index) vid: u32, \
         @builtin(instance_index) iid: u32, {ATTRS_PARAM}: {}) -> VOut {{",
        ir.vertex_source.attr_struct_name()
    );
    let mut p = Printer::new(Lang::Wgsl).with_depth(1);
    p.entry_body(&parsed.vertex);
    out.push_str(&p.finish());
    out.push_str("}\n");

    if !parsed.helpers.is_empty() {
        out.push('\n');
        let mut p = Printer::new(Lang::Wgsl);
        p.items(&parsed.helpers);
        out.push_str(&p.finish());
    }

    out.push_str("\n@fragment\nfn fragment_main(vin: VOut) -> @location(0) vec4<f32> {\n");
    let mut p = Printer::new(Lang::Wgsl).with_depth(1);
    p.entry_body(&parsed.fragment);
    out.push_str(&p.finish());
    out.push_str("}\n");
    Ok(out)
}

/// The WGSL spelling of an interface type.
fn wgsl_type(t: IrType) -> String {
    let scalar = |s: ScalarType| match s {
        ScalarType::F32 => "f32",
        ScalarType::U32 => "u32",
    };
    match t {
        IrType::Scalar(s) => scalar(s).to_string(),
        IrType::Vector { scalar: s, lanes } => format!("vec{lanes}<{}>", scalar(s)),
    }
}

fn push_field(out: &mut String, names: &Printer, f: &IrField) {
    let _ = writeln!(out, "    {}: {},", names.ident(f.name), wgsl_type(f.ty));
}

/// One `VOut` member. `position` is the clip-space builtin; every other varying
/// takes the next location, flat when integer-typed or marked `[[flat]]`.
fn push_varying(out: &mut String, names: &Printer, v: &Varying, location: &mut u32) {
    let name = names.ident(v.name);
    let ty = wgsl_type(v.ty);
    if v.attr.contains("[[position]]") {
        let _ = writeln!(out, "    @builtin(position) {name}: {ty},");
        return;
    }
    let flat = v.attr.contains("[[flat]]") || !is_float(v.ty);
    let interp = if flat { " @interpolate(flat)" } else { "" };
    let _ = writeln!(out, "    @location({location}){interp} {name}: {ty},");
    *location += 1;
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
    use crate::ir::module::{builtin_irs, quad_ir};

    fn validate(src: &str, caps: naga::valid::Capabilities) -> naga::Module {
        let module = match naga::front::wgsl::parse_str(src) {
            Ok(m) => m,
            Err(e) => panic!("{}\n{src}", e.emit_to_string(src)),
        };
        let mut v = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), caps);
        if let Err(e) = v.validate(&module) {
            panic!("{}\n{src}", e.emit_to_string(src));
        }
        module
    }

    #[test]
    fn every_builtin_is_valid_web_wgsl() {
        for ir in builtin_irs() {
            let src = emit_wgsl(&ir, WgslTarget::Web).unwrap();
            validate(&src, naga::valid::Capabilities::empty());
        }
    }

    #[test]
    fn every_builtin_is_valid_native_wgsl() {
        for ir in builtin_irs() {
            let src = emit_wgsl(&ir, WgslTarget::Native).unwrap();
            validate(&src, naga::valid::Capabilities::IMMEDIATES);
        }
    }

    #[test]
    fn interface_locations_follow_declaration_order() {
        let src = emit_wgsl(&quad_ir(), WgslTarget::Web).unwrap();
        let ir = quad_ir();
        for (i, f) in ir.attributes.iter().enumerate() {
            assert!(
                src.contains(&format!("@location({i}) {}:", f.name)),
                "{}:\n{src}",
                f.name
            );
        }
        assert!(src.contains("@builtin(position)"));
        assert!(src.contains("@group(0) @binding(0) var<uniform> uniforms: Uniforms;"));
        let native = emit_wgsl(&ir, WgslTarget::Native).unwrap();
        assert!(native.contains("var<immediate> uniforms: Uniforms;"));
    }
}
