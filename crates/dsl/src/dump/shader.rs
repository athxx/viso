//! The checked shaders a file declares: each one's bindings and its
//! functions and entries as the shader IR holds them, or why it has none.

use std::fmt::Write as _;

use viso_ende::JsonWriter;
use viso_shader::program::{Program, Span};

use super::{Dump, FileCx, strings};
use crate::frontend::Compiled;
use crate::syntax::{TextRange, TextSize};

struct Function {
    /// `fn#i`, `vertex` or `fragment`.
    label: String,
    header: String,
    lines: Vec<String>,
    range: TextRange,
    at: (u32, u32),
}

struct Binding {
    group: &'static str,
    name: String,
    ty: String,
    range: TextRange,
    at: (u32, u32),
}

struct Shader {
    name: String,
    symbol: Option<String>,
    /// The program's text, `None` when the shader has errors.
    program: Option<String>,
    brush: bool,
    bindings: Vec<Binding>,
    functions: Vec<Function>,
}

/// The shader IR of one file.
pub(super) struct ShaderFile {
    shaders: Vec<Shader>,
}

pub(super) fn shaders(cx: &FileCx<'_>, compiled: &Compiled) -> ShaderFile {
    let range = |span: Span| TextRange::new(TextSize::from(span.start), TextSize::from(span.end));
    let shaders = compiled
        .shaders
        .iter()
        .filter(|s| cx.focus.as_ref().is_none_or(|f| f.name == s.name))
        .map(|checked| {
            let Some(program) = &checked.program else {
                return Shader {
                    name: checked.name.clone(),
                    symbol: checked.symbol.map(|s| s.to_string()),
                    program: None,
                    brush: false,
                    bindings: Vec::new(),
                    functions: Vec::new(),
                };
            };
            Shader {
                name: checked.name.clone(),
                symbol: checked.symbol.map(|s| s.to_string()),
                program: Some(program.dump()),
                brush: checked.brush.is_some(),
                bindings: bindings(cx, program, range),
                functions: functions(cx, program, range),
            }
        })
        .collect();
    ShaderFile { shaders }
}

fn bindings(cx: &FileCx<'_>, program: &Program, range: impl Fn(Span) -> TextRange) -> Vec<Binding> {
    let groups = [
        ("uniform", &program.uniforms),
        ("instance", &program.instance),
        ("varying", &program.varyings),
        ("texture", &program.textures),
        ("sampler", &program.samplers),
    ];
    let mut out = Vec::new();
    for (group, bindings) in groups {
        for binding in bindings {
            let range = range(binding.span);
            out.push(Binding {
                group,
                name: binding.name.to_string(),
                ty: program.spell(binding.ty),
                range,
                at: cx.position(range),
            });
        }
    }
    out
}

fn functions(
    cx: &FileCx<'_>,
    program: &Program,
    range: impl Fn(Span) -> TextRange,
) -> Vec<Function> {
    let labelled = program
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| (format!("fn#{i}"), f))
        .chain(program.vertex.iter().map(|f| ("vertex".to_owned(), f)))
        .chain(program.fragment.iter().map(|f| ("fragment".to_owned(), f)));
    labelled
        .map(|(label, function)| {
            let (header, lines) = program.dump_function(function);
            let range = range(function.span);
            Function {
                label,
                header,
                lines,
                range,
                at: cx.position(range),
            }
        })
        .collect()
}

impl Dump for ShaderFile {
    fn write_text(&self, out: &mut String) {
        for shader in &self.shaders {
            match &shader.program {
                Some(program) => {
                    let brush = if shader.brush { " brush" } else { "" };
                    let _ = writeln!(out, "-- {}{brush}", shader.name);
                    out.push_str(program);
                }
                None => {
                    let _ = writeln!(out, "-- {} unchecked: it has errors", shader.name);
                }
            }
        }
    }

    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("shaders");
        w.begin_array();
        for shader in &self.shaders {
            w.begin_object();
            w.name("name");
            w.string(&shader.name);
            w.name("symbol_id");
            match &shader.symbol {
                Some(symbol) => w.string(symbol),
                None => w.null(),
            }
            w.name("checked");
            w.bool(shader.program.is_some());
            w.name("brush");
            w.bool(shader.brush);
            w.name("bindings");
            w.begin_array();
            for binding in &shader.bindings {
                w.begin_object();
                w.name("kind");
                w.string(binding.group);
                w.name("name");
                w.string(&binding.name);
                w.name("type");
                w.string(&binding.ty);
                super::hir::span(w, binding.range, binding.at);
                w.end_object();
            }
            w.end_array();
            w.name("functions");
            w.begin_array();
            for function in &shader.functions {
                w.begin_object();
                w.name("label");
                w.string(&function.label);
                w.name("signature");
                w.string(&function.header);
                w.name("body");
                strings(w, function.lines.iter().map(String::as_str));
                super::hir::span(w, function.range, function.at);
                w.end_object();
            }
            w.end_array();
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
}
