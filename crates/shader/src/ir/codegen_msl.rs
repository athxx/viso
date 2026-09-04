//! IR → Metal Shading Language codegen (architecture section 36 / AGENTS 19,
//! "backend codegen"; MSL first).
//!
//! [`emit_msl`] prints the declarations of one [`ShaderIr`] — the `InstanceIn`/
//! `VertexIn` attribute struct, the inline `Uniforms`, the `VOut` varyings, and
//! the two entry-point signatures — from the typed interface, then splices in the
//! IR's verbatim body fragments. [`emit_schema_attrs`] projects the *same* attribute
//! field list to an [`InstanceSchema`]. Because both read one field list, the MSL
//! struct and the validated schema cannot drift — the duplication the built-ins
//! used to carry (a hand-written `packed_float*` struct beside a parallel
//! `SchemaAttr` list, kept in lockstep only by name-order test assertions) is
//! gone.
//!
//! The generated text is byte-for-byte the historical hand-written MSL: the
//! declarations reproduce the exact spacing/alignment the built-ins shipped and
//! the bodies are carried verbatim, so migrating the source of truth to the IR
//! changes no produced Metal and needs no on-device recompile to trust (the
//! headless backend never compiles MSL — see `viso-msl-reserved-half`). A test
//! asserts `emit_msl(&quad_ir())` equals the frozen pre-migration text in
//! [`testdata`](crate::ir::testdata) (and the three others).

use viso_gpu::InstanceSchema;

use crate::ir::module::{IrField, ShaderIr, Varying, VertexSource};

/// Project the IR's attribute field list onto the [`InstanceSchema`] the pipeline
/// validates the derived `#[derive(GpuInstance)]` layout against. Same field
/// list, same order as the emitted MSL attribute struct.
///
/// Returns an owned attribute vector; the caller (`msl.rs`) caches it behind a
/// per-primitive `OnceLock` to hand out the `&'static` slice [`InstanceSchema`]
/// borrows — a one-time cold-path (pipeline-registration) cost.
pub fn emit_schema_attrs(ir: &ShaderIr) -> Vec<viso_gpu::SchemaAttr> {
    ir.schema_attrs()
}

/// Build an [`InstanceSchema`] from a `&'static` attribute slice previously
/// produced by [`emit_schema_attrs`] and cached to `'static`. Split from
/// [`emit_schema_attrs`] because `InstanceSchema` borrows its attributes for
/// `'static`, which only the caller's cache can provide.
pub fn schema_from_attrs(attrs: &'static [viso_gpu::SchemaAttr]) -> InstanceSchema {
    InstanceSchema { attributes: attrs }
}

/// Emit the complete MSL program for `ir`, byte-for-byte matching the historical
/// hand-written source for the built-ins.
pub fn emit_msl(ir: &ShaderIr) -> String {
    // The hand-written sources open with a newline right after `r#"` and close
    // with a trailing newline before `"#`; reproduce both.
    let mut out = String::with_capacity(1024);
    out.push('\n');
    out.push_str("#include <metal_stdlib>\n");
    out.push_str("using namespace metal;\n\n");

    emit_attr_struct(&mut out, ir);
    out.push('\n');
    emit_uniforms(&mut out, ir);
    out.push('\n');
    emit_vout(&mut out, ir);
    out.push('\n');

    emit_vertex_entry(&mut out, ir);

    if !ir.helpers.is_empty() {
        out.push('\n');
        out.push_str(ir.helpers);
        out.push('\n');
    }

    out.push('\n');
    emit_fragment_entry(&mut out, ir);

    out
}

/// `struct InstanceIn { ... };` / `struct VertexIn { ... };` — the attribute
/// struct, one `packed_*`/scalar field per IR attribute in declaration order.
fn emit_attr_struct(out: &mut String, ir: &ShaderIr) {
    out.push_str("struct ");
    out.push_str(ir.vertex_source.attr_struct_name());
    out.push_str(" {\n");
    for f in ir.attributes {
        push_field(out, f);
    }
    out.push_str("};\n");
}

/// `struct Uniforms { ... };` — the inline uniform block (viewport, etc.).
fn emit_uniforms(out: &mut String, ir: &ShaderIr) {
    out.push_str("struct Uniforms {\n");
    for f in ir.uniforms {
        push_field(out, f);
    }
    out.push_str("};\n");
}

/// `struct VOut { ... };` — the varyings. `position` carries its `[[position]]`
/// attribute; any varyings that carry a trailing line comment are column-aligned
/// exactly as the hand-written source, so the emitted text matches byte-for-byte.
fn emit_vout(out: &mut String, ir: &ShaderIr) {
    out.push_str("struct VOut {\n");

    // The comment column is the widest commented field's declaration length plus
    // a four-space minimum gap, matching how the built-ins were hand-aligned.
    let comment_col = ir
        .varyings
        .iter()
        .filter(|v| !v.comment.is_empty())
        .map(varying_decl_len)
        .max()
        .map(|w| w + 4);

    for v in ir.varyings {
        push_varying(out, v, comment_col);
    }
    out.push_str("};\n");
}

/// The length of a varying's `    <value-type> <name>;` declaration (four-space
/// indent + type + space + name + semicolon), used to align trailing comments.
fn varying_decl_len(v: &Varying) -> usize {
    4 + v.ty.msl_value_type().len() + 1 + v.name.len() + 1
}

/// One attribute/uniform field line: `    <field-type> <name>;`. Field types use
/// the packed spelling for vectors (`packed_float2`), bare for scalars.
fn push_field(out: &mut String, f: &IrField) {
    out.push_str("    ");
    out.push_str(&f.ty.msl_field_type());
    out.push(' ');
    out.push_str(f.name);
    out.push_str(";\n");
}

/// One `VOut` varying line: `    <value-type> <name><attr>;` plus an optional
/// column-aligned `// <comment>`. Value types use the unpacked spelling
/// (`float2`), since varyings are interpolated stage-in/out values, not storage.
fn push_varying(out: &mut String, v: &Varying, comment_col: Option<usize>) {
    out.push_str("    ");
    out.push_str(&v.ty.msl_value_type());
    out.push(' ');
    out.push_str(v.name);
    out.push_str(v.attr);
    out.push(';');
    if !v.comment.is_empty() {
        // Pad from the current line width to the aligned comment column.
        let width = varying_decl_len(v);
        let col = comment_col.unwrap_or(width);
        for _ in width..col {
            out.push(' ');
        }
        out.push_str("// ");
        out.push_str(v.comment);
    }
    out.push('\n');
}

/// The `vertex VOut vertex_main(...)` entry point: the signature reflects the
/// data source (per-instance reads `[[instance_id]]` + the instance buffer;
/// per-vertex reads only the vertex buffer), then the verbatim vertex body
/// indented four spaces.
fn emit_vertex_entry(out: &mut String, ir: &ShaderIr) {
    match ir.vertex_source {
        VertexSource::PerInstance => {
            out.push_str("vertex VOut vertex_main(uint vid [[vertex_id]],\n");
            out.push_str("                        uint iid [[instance_id]],\n");
            out.push_str(
                "                        const device InstanceIn* instances [[buffer(1)]],\n",
            );
            out.push_str("                        constant Uniforms& u [[buffer(0)]]) {\n");
        }
        VertexSource::PerVertex => {
            out.push_str("vertex VOut vertex_main(uint vid [[vertex_id]],\n");
            out.push_str("                        const device VertexIn* verts [[buffer(0)]],\n");
            out.push_str("                        constant Uniforms& u [[buffer(1)]]) {\n");
        }
    }
    push_body(out, ir.vertex_body);
    out.push_str("}\n");
}

/// The `fragment float4 fragment_main(...)` entry point. Textured primitives
/// (`texture_count == 1`) bind a `texture2d<float>` + `sampler`; untextured ones
/// take only `stage_in`.
fn emit_fragment_entry(out: &mut String, ir: &ShaderIr) {
    if ir.texture_count == 1 {
        out.push_str("fragment float4 fragment_main(VOut in [[stage_in]],\n");
        out.push_str("                              texture2d<float> tex [[texture(0)]],\n");
        out.push_str("                              sampler samp [[sampler(0)]]) {\n");
    } else {
        out.push_str("fragment float4 fragment_main(VOut in [[stage_in]]) {\n");
    }
    push_body(out, ir.fragment_body);
    out.push_str("}\n");
}

/// Splice a verbatim body fragment into an entry point: each non-empty line is
/// indented four spaces; empty lines stay empty (no trailing whitespace).
fn push_body(out: &mut String, body: &str) {
    for line in body.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ir::codegen_msl::emit_msl;
    use crate::ir::module::{glyphrun_ir, image_ir, mesh_ir, quad_ir};
    use crate::ir::testdata::{
        GLYPHRUN_MSL_ORIGINAL, IMAGE_MSL_ORIGINAL, MESH_MSL_ORIGINAL, QUAD_MSL_ORIGINAL,
    };

    // The oracle is the *frozen* pre-Slice-Q hand-written text (see `testdata`),
    // not the live `msl.rs` accessors — those now project from the same IR the
    // codegen does, so comparing against them would be a tautology. Byte equality
    // against the frozen text proves the IR migration produced identical Metal, so
    // no on-device recompile is needed to trust it.

    #[test]
    fn quad_msl_is_byte_equivalent() {
        assert_eq!(emit_msl(&quad_ir()), QUAD_MSL_ORIGINAL);
    }

    #[test]
    fn image_msl_is_byte_equivalent() {
        assert_eq!(emit_msl(&image_ir()), IMAGE_MSL_ORIGINAL);
    }

    #[test]
    fn glyphrun_msl_is_byte_equivalent() {
        assert_eq!(emit_msl(&glyphrun_ir()), GLYPHRUN_MSL_ORIGINAL);
    }

    #[test]
    fn mesh_msl_is_byte_equivalent() {
        assert_eq!(emit_msl(&mesh_ir()), MESH_MSL_ORIGINAL);
    }
}
