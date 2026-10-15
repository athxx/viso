//! The textual form of a [`Program`], for golden tests and tooling: its
//! records and bindings, then each function and entry with its locals and
//! its body, every expression parenthesized and every name resolved.

use std::fmt::Write as _;

use super::{Block, Builtin, Expr, ExprKind, Function, Place, Program, Root, Stage, Stmt, Ty};

impl Program {
    /// The program's textual dump.
    pub fn dump(&self) -> String {
        let mut out = format!("shader {}\n", self.name);
        for record in &self.records {
            let fields: Vec<String> = record
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, self.spell(f.ty)))
                .collect();
            let _ = writeln!(out, "record {} {{ {} }}", record.name, fields.join(", "));
        }
        let groups = [
            ("uniform", &self.uniforms),
            ("instance", &self.instance),
            ("varying", &self.varyings),
            ("texture", &self.textures),
            ("sampler", &self.samplers),
        ];
        for (kind, bindings) in groups {
            for (i, binding) in bindings.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "{kind}#{i} {}: {}",
                    binding.name,
                    self.spell(binding.ty)
                );
            }
        }
        for (i, function) in self.functions.iter().enumerate() {
            self.write_function(&mut out, &format!("fn#{i} "), function);
        }
        for entry in self.entries() {
            self.write_function(&mut out, "", entry);
        }
        out
    }

    /// The header and body lines of `function`.
    pub fn dump_function(&self, function: &Function) -> (String, Vec<String>) {
        let params: Vec<String> = function.locals[..function.params as usize]
            .iter()
            .map(|l| format!("{}: {}", l.name, self.spell(l.ty)))
            .collect();
        let mut header = format!(
            "{}({}) -> {}",
            function.name,
            params.join(", "),
            self.spell(function.ret)
        );
        if !function.builtins.is_empty() {
            let builtins: Vec<&str> = function.builtins.iter().map(|b| builtin(*b)).collect();
            let _ = write!(header, " builtins=[{}]", builtins.join(", "));
        }
        let mut lines = Vec::new();
        for local in &function.locals[function.params as usize..] {
            let keyword = if local.mutable { "local mut" } else { "local" };
            lines.push(format!(
                "{keyword} {}: {}",
                local.name,
                self.spell(local.ty)
            ));
        }
        Printer {
            program: self,
            function,
            lines: &mut lines,
        }
        .block(&function.body, 0);
        (header, lines)
    }

    fn write_function(&self, out: &mut String, label: &str, function: &Function) {
        let (header, lines) = self.dump_function(function);
        let _ = writeln!(out, "\n{label}{header}");
        for line in lines {
            let _ = writeln!(out, "  {line}");
        }
    }
}

fn builtin(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::VertexId => Builtin::names(Stage::Vertex)[0],
        Builtin::InstanceId => Builtin::names(Stage::Vertex)[1],
        Builtin::FragCoord => Builtin::names(Stage::Fragment)[0],
    }
}

struct Printer<'a> {
    program: &'a Program,
    function: &'a Function,
    lines: &'a mut Vec<String>,
}

const LANES: [char; 4] = ['x', 'y', 'z', 'w'];
const CHANNELS: [char; 4] = ['r', 'g', 'b', 'a'];

impl Printer<'_> {
    fn line(&mut self, depth: usize, text: String) {
        self.lines.push(format!("{}{text}", "  ".repeat(depth)));
    }

    fn local(&self, index: u32) -> String {
        self.function
            .locals
            .get(index as usize)
            .map_or_else(|| format!("l{index}"), |l| l.name.to_string())
    }

    fn block(&mut self, block: &Block, depth: usize) {
        for stmt in &block.0 {
            match stmt {
                Stmt::Let(local, value) => {
                    let text = format!("let {} = {}", self.local(*local), self.expr(value));
                    self.line(depth, text);
                }
                Stmt::Declare(local) => {
                    let text = format!("let {} = zero", self.local(*local));
                    self.line(depth, text);
                }
                Stmt::Assign(place, value) => {
                    let text = format!("{} = {}", self.place(place), self.expr(value));
                    self.line(depth, text);
                }
                Stmt::If(condition, then, otherwise) => {
                    let text = format!("if {}", self.expr(condition));
                    self.line(depth, text);
                    self.block(then, depth + 1);
                    if !otherwise.0.is_empty() {
                        self.line(depth, "else".to_owned());
                        self.block(otherwise, depth + 1);
                    }
                }
                Stmt::For(l) => {
                    let range = if l.inclusive { "..=" } else { ".." };
                    let guard = if l.guarded { " guarded" } else { "" };
                    let text = format!(
                        "for {} in {}{range}{} max={}{guard}",
                        self.local(l.var),
                        self.expr(&l.start),
                        self.expr(&l.end),
                        l.max
                    );
                    self.line(depth, text);
                    self.block(&l.body, depth + 1);
                }
                Stmt::Break => self.line(depth, "break".to_owned()),
                Stmt::Continue => self.line(depth, "continue".to_owned()),
                Stmt::Return(None) => self.line(depth, "return".to_owned()),
                Stmt::Return(Some(value)) => {
                    let text = format!("return {}", self.expr(value));
                    self.line(depth, text);
                }
                Stmt::Discard => self.line(depth, "discard".to_owned()),
            }
        }
    }

    fn place(&self, place: &Place) -> String {
        let (mut text, mut ty) = match place.root {
            Root::Local(i) => (
                self.local(i),
                self.function
                    .locals
                    .get(i as usize)
                    .map_or(Ty::Unit, |l| l.ty),
            ),
            Root::Varying(i) => {
                let binding = self.program.varyings.get(i as usize);
                (
                    format!("varying.{}", binding.map_or("?", |b| &b.name)),
                    binding.map_or(Ty::Unit, |b| b.ty),
                )
            }
        };
        for &step in &place.path {
            ty = self.step(&mut text, ty, step);
        }
        text
    }

    /// Appends the field, lane or column `step` of a `ty` to `text`; the
    /// type it selects.
    fn step(&self, text: &mut String, ty: Ty, step: u32) -> Ty {
        if let Some(fields) = self.program.fields(ty)
            && let Some((name, field)) = fields.get(step as usize)
        {
            let _ = write!(text, ".{name}");
            return *field;
        }
        match ty {
            Ty::Vector(scalar, _) => {
                text.push('.');
                text.push(LANES[step.min(3) as usize]);
                Ty::Scalar(scalar)
            }
            Ty::Color => {
                text.push('.');
                text.push(CHANNELS[step.min(3) as usize]);
                Ty::F32
            }
            Ty::Matrix(n) => {
                let _ = write!(text, "[{step}]");
                Ty::Vector(super::Scalar::F32, n)
            }
            _ => {
                let _ = write!(text, ".{step}");
                Ty::Unit
            }
        }
    }

    fn binding(bindings: &[super::Binding], kind: &str, index: u32) -> String {
        format!(
            "{kind}.{}",
            bindings.get(index as usize).map_or("?", |b| &b.name)
        )
    }

    fn args(&self, args: &[Expr]) -> String {
        args.iter()
            .map(|a| self.expr(a))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn expr(&self, expr: &Expr) -> String {
        let program = self.program;
        match &expr.kind {
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::I32(n) => format!("{n}i"),
            ExprKind::U32(n) => format!("{n}u"),
            ExprKind::F32(x) => format!("{x:?}"),
            ExprKind::Local(i) => self.local(*i),
            ExprKind::Uniform(i) => Self::binding(&program.uniforms, "uniform", *i),
            ExprKind::Instance(i) => Self::binding(&program.instance, "instance", *i),
            ExprKind::Varying(i) => Self::binding(&program.varyings, "varying", *i),
            ExprKind::Texture(i) => Self::binding(&program.textures, "texture", *i),
            ExprKind::Sampler(i) => Self::binding(&program.samplers, "sampler", *i),
            ExprKind::Unary(op, a) => format!("({}{})", op.symbol(), self.expr(a)),
            ExprKind::Binary(op, a, b) => {
                format!("({} {} {})", self.expr(a), op.symbol(), self.expr(b))
            }
            ExprKind::Construct(parts) => {
                format!("{}({})", program.spell(expr.ty), self.args(parts))
            }
            ExprKind::Swizzle(a, lanes) => {
                let count = expr.ty.lanes().map_or(4, |(_, n)| n as usize);
                let names = if a.ty == Ty::Color { CHANNELS } else { LANES };
                let letters: String = lanes[..count]
                    .iter()
                    .map(|&l| names[l.min(3) as usize])
                    .collect();
                format!("{}.{letters}", self.expr(a))
            }
            ExprKind::Member(a, index) => {
                let mut text = self.expr(a);
                self.step(&mut text, a.ty, *index);
                text
            }
            ExprKind::Index(a, i) => format!("{}[{}]", self.expr(a), self.expr(i)),
            ExprKind::Call(index, args) => {
                let name = program
                    .functions
                    .get(*index as usize)
                    .map_or("?", |f| &f.name);
                format!("{name}({})", self.args(args))
            }
            ExprKind::Intrinsic(intrinsic, args) => {
                format!("{}({})", intrinsic.name(), self.args(args))
            }
            ExprKind::Convert(a) => format!("{}({})", program.spell(expr.ty), self.expr(a)),
        }
    }
}
