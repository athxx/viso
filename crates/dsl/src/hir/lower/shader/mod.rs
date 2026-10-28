//! `shader` declarations and `@shader_value` records: each shader's
//! interface and bodies checked against the closed shader type set, the
//! shader syntax subset and the render profile's stage rules, and lowered into
//! a [`viso_shader::program::Program`].
//!
//! A shader scopes its own names — its locals, its interface members, its
//! `fn`s, the intrinsics — and names a type by the closed set or a
//! `@shader_value` record the resolver bound. Host-only types are `E8101`,
//! `F64` is `E8102`, a loop without a static bound `E8103`, a uniform or
//! instance member without a buffer layout `E8104`, a construct outside the
//! subset `E8105` and a stage rule `E8106`.

mod body;

use std::collections::{HashMap, HashSet};

use viso_shader::program::brush::{RECT_POS, RECT_SIZE};
use viso_shader::program::{
    AddressMode, Binding, BrushLayout, FilterMode, Function, ParamKind, Program, SamplerDesc,
    Scalar, Span, Stage, Texel, Ty,
};

use super::{ModuleEnv, name_of};
use crate::ast::{AstNode, RecordDecl, ShaderDecl, ShaderMember, ShaderStage, TypePath};
use crate::diag::Diagnostic;
use crate::hir::Ty as HostTy;
use crate::hir::infer::FieldInfo;
use crate::resolve::SymbolId;
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange, TextSize};

/// A checked shader: its program, when it checked cleanly, and its layout as a
/// brush, when it is one.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedShader {
    pub name: String,
    pub symbol: Option<SymbolId>,
    /// The program; `None` when the shader has errors.
    pub program: Option<Program>,
    /// Where its parameters go as a brush; `None` when it has errors or breaks
    /// the brush profile.
    pub brush: Option<BrushLayout>,
    /// How it breaks the brush profile, reported only where it paints: a
    /// shader no `ShaderView` paints with need not be a brush.
    pub(super) brush_errors: Vec<Diagnostic>,
}

/// A value a view paints a node with a shader by: a `ShaderView`'s `shader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderPaint {
    /// The component whose view holds it; `None` in a fragment.
    pub component: Option<SymbolId>,
    /// The value's span.
    pub at: TextRange,
    /// The shader whose parameters it gives.
    pub shader: SymbolId,
}

/// A type annotation as written, its head bound by the resolver.
#[derive(Debug, Clone)]
pub(super) struct TypeRef {
    head: String,
    segments: usize,
    args: Vec<TypeRef>,
    symbol: Option<SymbolId>,
    range: TextRange,
    tuple: bool,
}

impl TypeRef {
    /// The annotation `node` (a type path or a tuple type), named through the
    /// resolver's bindings `nominal`.
    pub(super) fn of(node: &SyntaxNode, nominal: &HashMap<TextRange, HostTy>) -> TypeRef {
        let range = node.text_range();
        let Some(path) = TypePath::cast(node.clone()) else {
            return TypeRef {
                head: node.text().trim().to_owned(),
                segments: 0,
                args: Vec::new(),
                symbol: None,
                range,
                tuple: true,
            };
        };
        let segments: Vec<_> = path.segments().collect();
        let symbol = segments
            .first()
            .and_then(|head| match nominal.get(&head.text_range()) {
                Some(HostTy::Named(symbol, ..)) => Some(*symbol),
                _ => None,
            });
        let args = node
            .children()
            .into_iter()
            .rfind(|n| n.kind() == SyntaxKind::TypePathSegment)
            .into_iter()
            .flat_map(|segment| segment.children())
            .filter(|n| n.kind() == SyntaxKind::GenericArgs)
            .flat_map(|args| args.children())
            .filter(|n| matches!(n.kind(), SyntaxKind::TypePath | SyntaxKind::TupleType))
            .map(|arg| TypeRef::of(&arg, nominal))
            .collect();
        TypeRef {
            head: segments.last().map(|s| s.text()).unwrap_or_default(),
            segments: segments.len(),
            args,
            symbol,
            range,
            tuple: false,
        }
    }
}

/// A `@shader_value` record: its fields' annotations.
#[derive(Debug, Clone)]
pub(super) struct ShaderValueDecl {
    pub(super) name: String,
    pub(super) at: TextRange,
    pub(super) fields: Vec<(String, TextRange, TypeRef)>,
}

impl ShaderValueDecl {
    /// `record`'s shape, when it is marked `@shader_value`.
    pub(super) fn of(
        record: &RecordDecl,
        nominal: &HashMap<TextRange, HostTy>,
    ) -> Option<ShaderValueDecl> {
        let marked = crate::ast::decl_attributes(record.syntax())
            .iter()
            .any(|(name, ..)| name == "shader_value");
        if !marked {
            return None;
        }
        let name = record.name()?;
        let fields = record
            .fields()
            .filter_map(|field| {
                let at = field.name()?;
                let ty = field.ty()?;
                Some((
                    at.text(),
                    at.text_range(),
                    TypeRef::of(ty.syntax(), nominal),
                ))
            })
            .collect();
        Some(ShaderValueDecl {
            name: name.text(),
            at: name.text_range(),
            fields,
        })
    }
}

/// The parameters of the shader `decl` as the fields of the record literal its
/// name heads: every instance member but the box the renderer writes, typed as
/// the host value its parameter kind converts from, then every image texture,
/// an `ImageAsset`. A member of no parameter kind is no field; the brush
/// profile reports it where the shader paints.
pub(super) fn parameters(decl: &ShaderDecl) -> Vec<FieldInfo> {
    let textures = decl.members().filter_map(|member| {
        let ShaderMember::Texture(m) = member else {
            return None;
        };
        let name = m.name()?;
        let ty: String = m
            .ty()?
            .syntax()
            .text()
            .to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        (ty == "Texture2D<Vec4F32>").then(|| FieldInfo {
            name: name.text(),
            ty: HostTy::named(crate::resolve::prelude::image_record()),
            has_default: false,
            declared_at: name.text_range(),
        })
    });
    let instance: Vec<FieldInfo> = decl
        .members()
        .filter_map(|member| {
            let ShaderMember::Instance(m) = member else {
                return None;
            };
            let name = m.name()?;
            let text = name.text();
            if text == RECT_POS || text == RECT_SIZE {
                return None;
            }
            let path = m.ty()?;
            let mut segments = path.segments();
            let head = segments.next()?;
            if segments.next().is_some() || path.syntax().text().contains('<') {
                return None;
            }
            let kind = ParamKind::of(Ty::from_name(&head.text())?)?;
            Some(FieldInfo {
                name: text,
                ty: host_ty(kind),
                has_default: false,
                declared_at: name.text_range(),
            })
        })
        .collect();
    instance.into_iter().chain(textures).collect()
}

/// The host type a parameter of `kind` takes its value from.
fn host_ty(kind: ParamKind) -> HostTy {
    let vector = |path| HostTy::native(viso_behavior::native::NativeId::of(path));
    match kind {
        ParamKind::F32 => HostTy::F32,
        ParamKind::I32 => HostTy::I32,
        ParamKind::U32 => HostTy::U32,
        ParamKind::Vec2F32 => vector(viso_behavior::native::VEC2_PATH),
        ParamKind::Vec3F32 => vector(viso_behavior::native::VEC3_PATH),
        ParamKind::ColorLinear => HostTy::Color,
    }
}

/// What a type annotation names in a shader.
enum Class {
    Shader(Ty),
    /// A `@shader_value` record.
    Record(SymbolId),
}

/// Why a type annotation names no shader type: the code and message.
struct Issue(&'static str, String);

/// What `r` names in a shader.
fn classify(r: &TypeRef, env: &ModuleEnv<'_>) -> Result<Class, Issue> {
    let host = |what: &str| {
        Issue(
            "E8101",
            format!(
                "`{what}` is a host type; a shader uses the shader types and `@shader_value` records"
            ),
        )
    };
    if r.tuple {
        return Err(host(&r.head));
    }
    if r.segments == 1 {
        if r.head == "Texture2D" {
            return match r.args.as_slice() {
                [arg] => match classify(arg, env) {
                    Ok(Class::Shader(Ty::Scalar(Scalar::F32))) => {
                        Ok(Class::Shader(Ty::Texture(Texel::F32)))
                    }
                    Ok(Class::Shader(Ty::VEC4)) => Ok(Class::Shader(Ty::Texture(Texel::Vec4F32))),
                    _ => Err(Issue(
                        "E2103",
                        "a texture is a `Texture2D<F32>` or a `Texture2D<Vec4F32>`".into(),
                    )),
                },
                _ => Err(Issue(
                    "E2103",
                    "`Texture2D` takes its texel type: `Texture2D<Vec4F32>`".into(),
                )),
            };
        }
        if r.head == "F64" {
            return Err(Issue("E8102", "a shader has no `F64`; use `F32`".into()));
        }
        if let Some(ty) = Ty::from_name(&r.head) {
            return if r.args.is_empty() {
                Ok(Class::Shader(ty))
            } else {
                Err(Issue(
                    "E2103",
                    format!("`{}` takes no type arguments", r.head),
                ))
            };
        }
    }
    if let Some(symbol) = r.symbol {
        if env.decls.shader_values.contains_key(&symbol) {
            return Ok(Class::Record(symbol));
        }
        if env.decls.types.records.contains_key(&symbol) {
            return Err(Issue(
                "E8101",
                format!("record `{}` is not a `@shader_value` record", r.head),
            ));
        }
        return Err(host(&r.head));
    }
    let known_host = !matches!(HostTy::from_builtin_name(&r.head), Ok(None))
        || matches!(
            r.head.as_str(),
            "List"
                | "Map"
                | "Option"
                | "Result"
                | "Set"
                | "Text"
                | "Int"
                | "Vec2"
                | "Vec3"
                | "Vec4"
        );
    if known_host {
        Err(host(&r.head))
    } else {
        Err(Issue(
            "E2001",
            format!("cannot find type `{}` in this shader", r.head),
        ))
    }
}

/// Checks the `@shader_value` record `symbol`: its fields are value types of
/// the shader set or other `@shader_value` records, and it does not contain
/// itself.
pub(super) fn check_record(
    symbol: SymbolId,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(decl) = env.decls.shader_values.get(&symbol) else {
        return;
    };
    for (name, at, r) in &decl.fields {
        match classify(r, env) {
            Ok(Class::Shader(ty)) if !ty.is_value() || ty == Ty::VertexOutput => {
                diagnostics.push(Diagnostic::error(
                    "E8105",
                    r.range,
                    format!(
                        "field `{name}` cannot be a `{}`: a record field is a value",
                        r.head
                    ),
                ));
            }
            Ok(_) => {}
            Err(Issue(code, message)) => {
                diagnostics.push(Diagnostic::error(code, r.range, message));
            }
        }
        let _ = at;
    }
    // A record reaching itself through its fields has no finite layout.
    let mut stack = vec![symbol];
    let mut seen = HashSet::new();
    while let Some(next) = stack.pop() {
        let Some(decl) = env.decls.shader_values.get(&next) else {
            continue;
        };
        for (_, _, r) in &decl.fields {
            if let Ok(Class::Record(inner)) = classify(r, env) {
                if inner == symbol {
                    diagnostics.push(Diagnostic::error(
                        "E8105",
                        env.decls.shader_values[&symbol].at,
                        format!(
                            "record `{}` contains itself",
                            env.decls.shader_values[&symbol].name
                        ),
                    ));
                    return;
                }
                if seen.insert(inner) {
                    stack.push(inner);
                }
            }
        }
    }
}

/// A shader interface member a name reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Global {
    Uniform(u32),
    Instance(u32),
    Varying(u32),
    Texture(u32),
    Sampler(u32),
}

/// A shader `fn`'s signature, typed before any body so calls in any order
/// type against it.
struct Signature {
    params: Vec<Ty>,
    ret: Ty,
}

/// One shader being checked.
struct Shader<'e, 'p> {
    env: &'e ModuleEnv<'p>,
    diagnostics: &'e mut Vec<Diagnostic>,
    errors_before: usize,
    program: Program,
    /// Each record interned so far, by its symbol.
    records: HashMap<SymbolId, u32>,
    /// Records whose fields are being interned.
    interning: HashSet<SymbolId>,
    globals: HashMap<String, (Global, TextRange)>,
    /// Each `fn` by name: its declaration index and signature.
    fns: HashMap<String, (u32, Signature)>,
    /// Each sampler's state, in declaration order.
    samplers: Vec<SamplerDesc>,
}

pub(super) fn span(range: TextRange) -> Span {
    Span {
        start: range.start().to_u32(),
        end: range.end().to_u32(),
    }
}

impl Shader<'_, '_> {
    fn error(&mut self, code: &'static str, range: TextRange, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::error(code, range, message));
    }

    /// The shader type `node` annotates, reporting why when it names none.
    fn ty(&mut self, node: &SyntaxNode) -> Option<Ty> {
        let r = TypeRef::of(node, &self.env.scope.nominal);
        match classify(&r, self.env) {
            Ok(Class::Shader(ty)) => Some(ty),
            Ok(Class::Record(symbol)) => self.record(symbol),
            Err(Issue(code, message)) => {
                self.error(code, r.range, message);
                None
            }
        }
    }

    /// The program's index of `symbol`'s record, interned with the records its
    /// fields reach first; `None` when a field names no shader type (its
    /// record's declaration reports that) or the record contains itself.
    fn record(&mut self, symbol: SymbolId) -> Option<Ty> {
        if let Some(&index) = self.records.get(&symbol) {
            return Some(Ty::Record(index));
        }
        if !self.interning.insert(symbol) {
            return None;
        }
        let decl = self.env.decls.shader_values.get(&symbol)?.clone();
        let mut fields = Vec::with_capacity(decl.fields.len());
        for (name, at, r) in &decl.fields {
            let ty = match classify(r, self.env).ok()? {
                Class::Shader(ty) if ty.is_value() && ty != Ty::VertexOutput => ty,
                Class::Shader(_) => return None,
                Class::Record(inner) => self.record(inner)?,
            };
            fields.push(Binding {
                name: name.as_str().into(),
                ty,
                span: span(*at),
            });
        }
        self.interning.remove(&symbol);
        let index = self.program.records.len() as u32;
        self.program.records.push(viso_shader::program::Record {
            name: decl.name.as_str().into(),
            fields,
            span: span(decl.at),
        });
        self.records.insert(symbol, index);
        Some(Ty::Record(index))
    }

    /// The shader type of an annotation node child of `node`.
    fn annotation(&mut self, node: &SyntaxNode) -> Option<Ty> {
        let ty = node
            .children()
            .into_iter()
            .find(|c| matches!(c.kind(), SyntaxKind::TypePath | SyntaxKind::TupleType))?;
        self.ty(&ty)
    }

    fn declare(&mut self, name: &str, at: TextRange, global: Global) -> bool {
        if let Some((_, first)) = self.globals.get(name) {
            let first = *first;
            let mut d = Diagnostic::error(
                "E2002",
                at,
                format!("`{name}` is already declared in this shader"),
            );
            d.related
                .push(crate::diag::Related::new(first, "first declared here"));
            self.diagnostics.push(d);
            return false;
        }
        self.globals.insert(name.to_owned(), (global, at));
        true
    }

    /// Whether the uniform or instance member `name` of `ty` has a buffer
    /// layout, reporting why not.
    fn laid_out(&mut self, name: &str, at: TextRange, ty: Ty) -> bool {
        let problem = match ty {
            Ty::Texture(_) | Ty::Sampler => Some((
                "E8105",
                format!("`{name}` is a binding; declare it with `texture` or `sampler`"),
            )),
            Ty::VertexOutput => Some((
                "E8106",
                "`VertexOutput` is what a vertex entry returns".to_owned(),
            )),
            _ if holds_bool(&self.program, ty) => Some((
                "E8104",
                format!("`{name}` holds a `Bool`, which has no buffer layout; use `U32`"),
            )),
            _ => None,
        };
        match problem {
            Some((code, message)) => {
                self.error(code, at, message);
                false
            }
            None => true,
        }
    }
}

fn holds_bool(program: &Program, ty: Ty) -> bool {
    match ty {
        Ty::Scalar(Scalar::Bool) | Ty::Vector(Scalar::Bool, _) => true,
        Ty::Record(i) => program.records[i as usize]
            .fields
            .iter()
            .any(|f| holds_bool(program, f.ty)),
        _ => false,
    }
}

/// Checks `decl` and lowers it into a program.
pub(super) fn check(
    decl: &ShaderDecl,
    symbol: Option<SymbolId>,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> CheckedShader {
    let name = name_of(decl.name());
    let errors_before = diagnostics.len();
    let mut sh = Shader {
        env,
        diagnostics,
        errors_before,
        program: Program {
            name: name.as_str().into(),
            ..Program::default()
        },
        records: HashMap::new(),
        interning: HashSet::new(),
        globals: HashMap::new(),
        fns: HashMap::new(),
        samplers: Vec::new(),
    };
    if let Some(generics) = decl.generic_params() {
        sh.error(
            "E8105",
            generics.text_range(),
            "a shader takes no generic parameters",
        );
    }
    let members: Vec<ShaderMember> = decl.members().collect();
    interface(&mut sh, &members);
    let fns = signatures(&mut sh, &members);
    let mut functions = Vec::with_capacity(fns.len());
    let mut calls = Vec::with_capacity(fns.len());
    for f in &fns {
        let (function, called) = body::function(&mut sh, f);
        functions.push(function);
        calls.push(called);
    }
    let mut entries: [Option<(Function, TextRange)>; 2] = [None, None];
    for member in &members {
        let ShaderMember::Entry(entry) = member else {
            continue;
        };
        let at = entry
            .stage_token()
            .map_or(entry.syntax().text_range(), |t| t.text_range());
        let stage = match entry.stage() {
            Some(ShaderStage::Vertex) => Stage::Vertex,
            Some(ShaderStage::Fragment) => Stage::Fragment,
            Some(ShaderStage::Compute) => {
                sh.error("E8106", at, "the render profile has no compute stage");
                continue;
            }
            None => continue,
        };
        let slot = &mut entries[stage as usize];
        if let Some((_, first)) = slot {
            let mut d = Diagnostic::error(
                "E8106",
                at,
                format!("a shader has one `{}` entry", stage_word(stage)),
            );
            d.related
                .push(crate::diag::Related::new(*first, "first declared here"));
            sh.diagnostics.push(d);
            continue;
        }
        let (function, _) = body::entry(&mut sh, entry, stage);
        *slot = function.map(|f| (f, at));
    }
    let at = decl
        .name()
        .map_or(decl.syntax().text_range(), |t| t.text_range());
    for (stage, entry) in [Stage::Vertex, Stage::Fragment].into_iter().zip(&entries) {
        if entry.is_none() && sh.diagnostics.len() == errors_before {
            sh.error(
                "E8106",
                at,
                format!("the shader has no `{}` entry", stage_word(stage)),
            );
        }
    }
    let [vertex, fragment] = entries;
    sh.program.vertex = vertex.map(|(f, _)| f);
    sh.program.fragment = fragment.map(|(f, _)| f);
    order_functions(&mut sh, functions, calls);

    let clean = !sh.diagnostics[sh.errors_before..]
        .iter()
        .any(|d| d.severity == crate::diag::Severity::Error);
    let samplers = std::mem::take(&mut sh.samplers);
    let program = clean.then_some(sh.program);
    if let Some(program) = &program
        && let Err(errors) = program.validate()
    {
        // The checker and the validator disagree: a checker bug, reported so
        // the program is never used.
        for mut e in errors {
            debug_assert!(false, "{}: {}", e.code, e.message);
            e.message.insert_str(0, "internal: ");
            sh.diagnostics.push(e);
        }
        return CheckedShader {
            name,
            symbol,
            program: None,
            brush: None,
            brush_errors: Vec::new(),
        };
    }
    let (brush, brush_errors) = match &program {
        Some(program) => match BrushLayout::new(program, &program.interface()) {
            Ok(layout) => (Some(BrushLayout { samplers, ..layout }), Vec::new()),
            Err(mut errors) => {
                // A member the shader lacks has no span: the shader's name
                // stands for it.
                for e in &mut errors {
                    if e.primary == TextRange::empty(TextSize::ZERO) {
                        e.primary = at;
                    }
                }
                (None, errors)
            }
        },
        None => (None, Vec::new()),
    };
    CheckedShader {
        name,
        symbol,
        program,
        brush,
        brush_errors,
    }
}

/// Reports, once each, how a shader a view paints with breaks the brush
/// profile: in the shader's module, pointing at the first `ShaderView` that
/// paints with it. `brushes` holds each use and its module.
pub(super) fn check_brushes(
    shaders: &[CheckedShader],
    brushes: &[(usize, ShaderPaint)],
    decls: &super::Declarations,
    per_module: &mut [Vec<Diagnostic>],
) {
    let mut reported = HashSet::new();
    for &(
        module,
        ShaderPaint {
            at, shader: symbol, ..
        },
    ) in brushes
    {
        if !reported.insert(symbol) {
            continue;
        }
        let Some(shader) = shaders.iter().find(|s| s.symbol == Some(symbol)) else {
            continue;
        };
        let Some(&home) = decls.homes.get(&symbol) else {
            continue;
        };
        for error in &shader.brush_errors {
            let mut d = error.clone();
            let label = format!("`{}` paints this view", shader.name);
            d.related.push(if module == home {
                crate::diag::Related::new(at, label)
            } else {
                crate::diag::Related::in_module(decls.module_paths[module].as_str(), at, label)
            });
            per_module[home].push(d);
        }
    }
}

const fn stage_word(stage: Stage) -> &'static str {
    match stage {
        Stage::Vertex => "vertex",
        Stage::Fragment => "fragment",
    }
}

/// Checks the bindings into the program's interface.
fn interface(sh: &mut Shader<'_, '_>, members: &[ShaderMember]) {
    for member in members {
        let (name, node) = match member {
            ShaderMember::Uniform(m) => (m.name(), m.syntax()),
            ShaderMember::Instance(m) => (m.name(), m.syntax()),
            ShaderMember::Varying(m) => (m.name(), m.syntax()),
            ShaderMember::Texture(m) => (m.name(), m.syntax()),
            ShaderMember::Sampler(m) => (m.name(), m.syntax()),
            ShaderMember::Fn(_) | ShaderMember::Entry(_) => continue,
        };
        let state = member_attributes(sh, member, node);
        let Some(name) = name else {
            continue;
        };
        let (text, at) = (name.text(), name.text_range());
        let Some(ty) = sh.annotation(node) else {
            continue;
        };
        let binding = Binding {
            name: text.as_str().into(),
            ty,
            span: span(at),
        };
        let p = &sh.program;
        let global = match member {
            ShaderMember::Uniform(_) => Global::Uniform(p.uniforms.len() as u32),
            ShaderMember::Instance(_) => Global::Instance(p.instance.len() as u32),
            ShaderMember::Varying(_) => Global::Varying(p.varyings.len() as u32),
            ShaderMember::Texture(_) => Global::Texture(p.textures.len() as u32),
            _ => Global::Sampler(p.samplers.len() as u32),
        };
        let ok = match global {
            Global::Uniform(_) | Global::Instance(_) => sh.laid_out(&text, at, ty),
            Global::Varying(_) => {
                let ok = matches!(ty.lanes(), Some((s, _)) if s.is_numeric());
                if !ok {
                    let spelled = sh.program.spell(ty);
                    sh.error("E8106", at, format!("varying `{text}` is a `{spelled}`; a varying is a numeric scalar or vector"));
                }
                ok
            }
            Global::Texture(_) => {
                let ok = matches!(ty, Ty::Texture(_));
                if !ok {
                    sh.error(
                        "E2103",
                        at,
                        "a texture is a `Texture2D<F32>` or a `Texture2D<Vec4F32>`",
                    );
                }
                ok
            }
            Global::Sampler(_) => {
                let ok = ty == Ty::Sampler;
                if !ok {
                    sh.error("E2103", at, "a sampler is a `Sampler`");
                }
                ok
            }
        };
        if !ok || !sh.declare(&text, at, global) {
            continue;
        }
        let p = &mut sh.program;
        match global {
            Global::Uniform(_) => p.uniforms.push(binding),
            Global::Instance(_) => p.instance.push(binding),
            Global::Varying(_) => p.varyings.push(binding),
            Global::Texture(_) => p.textures.push(binding),
            Global::Sampler(_) => {
                p.samplers.push(binding);
                sh.samplers.push(state);
            }
        }
    }
}

/// Checks the attributes on the binding member `member`: `@sampler` gives a
/// sampler its state, which is bilinear and clamped to the edge without one;
/// no other member takes it.
fn member_attributes(
    sh: &mut Shader<'_, '_>,
    member: &ShaderMember,
    node: &SyntaxNode,
) -> SamplerDesc {
    let mut state = SamplerDesc::LINEAR_CLAMP;
    let mut stated = false;
    let attrs: Vec<SyntaxNode> =
        std::iter::successors(node.prev_sibling(), SyntaxNode::prev_sibling)
            .take_while(|n| n.kind() == SyntaxKind::Attribute)
            .collect();
    for attr in attrs.iter().rev() {
        let path = attr
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::PathExpr)
            .map(|p| p.text().to_string().trim().to_owned());
        if path.as_deref() != Some("sampler") {
            continue;
        }
        if !matches!(member, ShaderMember::Sampler(_)) {
            sh.error(
                "E8105",
                attr.text_range(),
                "`@sampler` sets a `sampler` member's state",
            );
            continue;
        }
        if std::mem::replace(&mut stated, true) {
            sh.error(
                "E8105",
                attr.text_range(),
                "a sampler takes one `@sampler`; name both settings in it",
            );
            continue;
        }
        sampler_settings(sh, attr, &mut state);
    }
    state
}

/// Reads `@sampler(filter: nearest | linear, address: clamp | repeat |
/// mirror)` into `state`, each setting at most once.
fn sampler_settings(sh: &mut Shader<'_, '_>, attr: &SyntaxNode, state: &mut SamplerDesc) {
    let args = attr
        .children()
        .into_iter()
        .filter(|c| c.kind() == SyntaxKind::ArgumentList)
        .flat_map(|list| list.children())
        .filter(|c| c.kind() == SyntaxKind::Argument);
    let (mut filter, mut address) = (false, false);
    for arg in args {
        let tokens: Vec<_> = arg
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| !t.kind().is_trivia())
            .collect();
        let label = match tokens.as_slice() {
            [label, colon, ..] if colon.kind() == SyntaxKind::Colon => label.text().to_string(),
            _ => {
                sh.error(
                    "E8105",
                    arg.text_range(),
                    "a `@sampler` setting is `filter: …` or `address: …`",
                );
                continue;
            }
        };
        let value = match arg.children().as_slice() {
            [value] if value.kind() == SyntaxKind::PathExpr => {
                value.text().to_string().trim().to_owned()
            }
            _ => String::new(),
        };
        let (seen, setting, choices) = match label.as_str() {
            "filter" => (
                &mut filter,
                match value.as_str() {
                    "nearest" => Some(Setting::Filter(FilterMode::Nearest)),
                    "linear" => Some(Setting::Filter(FilterMode::Linear)),
                    _ => None,
                },
                "`nearest` or `linear`",
            ),
            "address" => (
                &mut address,
                match value.as_str() {
                    "clamp" => Some(Setting::Address(AddressMode::ClampToEdge)),
                    "repeat" => Some(Setting::Address(AddressMode::Repeat)),
                    "mirror" => Some(Setting::Address(AddressMode::Mirror)),
                    _ => None,
                },
                "`clamp`, `repeat` or `mirror`",
            ),
            _ => {
                sh.error(
                    "E8105",
                    arg.text_range(),
                    format!(
                        "`{label}` is no sampler setting; a sampler sets `filter` and `address`"
                    ),
                );
                continue;
            }
        };
        if std::mem::replace(seen, true) {
            sh.error("E8105", arg.text_range(), format!("`{label}` is set twice"));
            continue;
        }
        match setting {
            Some(Setting::Filter(f)) => state.filter = f,
            Some(Setting::Address(a)) => state.address = a,
            None => sh.error(
                "E8105",
                arg.text_range(),
                format!("a sampler's `{label}` is {choices}"),
            ),
        }
    }
}

/// One `@sampler` setting.
enum Setting {
    Filter(FilterMode),
    Address(AddressMode),
}

/// Types every `fn`'s signature, so a body calls a `fn` declared after it.
fn signatures(sh: &mut Shader<'_, '_>, members: &[ShaderMember]) -> Vec<crate::ast::ShaderFn> {
    let mut fns = Vec::new();
    for member in members {
        let ShaderMember::Fn(f) = member else {
            continue;
        };
        let Some(name) = f.name() else {
            continue;
        };
        let (text, at) = (name.text(), name.text_range());
        if let Some((_, first)) = sh.globals.get(&text) {
            let first = *first;
            let mut d = Diagnostic::error(
                "E2002",
                at,
                format!("`{text}` is already declared in this shader"),
            );
            d.related
                .push(crate::diag::Related::new(first, "first declared here"));
            sh.diagnostics.push(d);
            continue;
        }
        if sh.fns.contains_key(&text) {
            sh.error(
                "E2002",
                at,
                format!("`{text}` is already declared in this shader"),
            );
            continue;
        }
        let params = f
            .params()
            .iter()
            .map(|p| sh.annotation(p.syntax()).unwrap_or(Ty::Unit))
            .collect();
        let ret = match f.return_type() {
            Some(ret) => sh.annotation(ret.syntax()).unwrap_or(Ty::Unit),
            None => Ty::Unit,
        };
        let index = fns.len() as u32;
        sh.fns.insert(text, (index, Signature { params, ret }));
        fns.push(f.clone());
    }
    fns
}

/// Puts each `fn` after the ones it calls, reporting recursion (`E8105`), and
/// renumbers the calls to the new order.
fn order_functions(
    sh: &mut Shader<'_, '_>,
    functions: Vec<Function>,
    calls: Vec<Vec<(u32, TextRange)>>,
) {
    let n = functions.len();
    // 0 unvisited, 1 on the walk, 2 placed.
    let mut state = vec![0u8; n];
    let mut order = Vec::with_capacity(n);
    for root in 0..n {
        if state[root] != 0 {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        state[root] = 1;
        while let Some(&mut (f, ref mut next)) = stack.last_mut() {
            if let Some(&(callee, at)) = calls[f].get(*next) {
                *next += 1;
                let callee = callee as usize;
                match state[callee] {
                    0 => {
                        state[callee] = 1;
                        stack.push((callee, 0));
                    }
                    1 => {
                        let name = functions[callee].name.clone();
                        sh.error("E8105", at, format!("this call makes `{name}` recursive; a shader function cannot recurse"));
                    }
                    _ => {}
                }
            } else {
                state[f] = 2;
                order.push(f);
                stack.pop();
            }
        }
    }
    let mut position = vec![0u32; n];
    for (new, &old) in order.iter().enumerate() {
        position[old] = new as u32;
    }
    let mut slots: Vec<Option<Function>> = functions.into_iter().map(Some).collect();
    sh.program.functions = order.iter().filter_map(|&old| slots[old].take()).collect();
    for f in sh
        .program
        .functions
        .iter_mut()
        .chain(sh.program.vertex.iter_mut())
        .chain(sh.program.fragment.iter_mut())
    {
        body::renumber_calls(&mut f.body, &position);
    }
}
