//! A user shader as owned, typed, structured data: its interface — uniforms,
//! instance fields, varyings, textures, samplers — its functions and its
//! vertex and fragment entries, every expression typed and every loop
//! bounded.
//!
//! A front end builds a [`Program`] (the DSL lowers a checked `shader`
//! declaration into one); [`validate`](Program::validate) rechecks it whatever
//! built it, so a backend or the reference interpreter never meets an
//! ill-typed tree.
//!
//! - [`ty`] — the closed type set;
//! - [`ops`] — the operators and intrinsics, and the types they take.

pub mod ops;
pub mod ty;
mod validate;

pub use ops::{BinaryOp, Intrinsic, StageUse, UnaryOp};
pub use ty::{Scalar, Texel, Ty};
pub use validate::ProgramError;

/// A byte range of the source a program was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// A user shader.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Program {
    /// The shader's name.
    pub name: Box<str>,
    /// The `@shader_value` records it uses, by [`Ty::Record`] index.
    pub records: Vec<Record>,
    pub uniforms: Vec<Binding>,
    pub instance: Vec<Binding>,
    /// Written by the vertex entry, read by the fragment entry.
    pub varyings: Vec<Binding>,
    pub textures: Vec<Binding>,
    pub samplers: Vec<Binding>,
    /// Its `fn`s, by [`ExprKind::Call`] index.
    pub functions: Vec<Function>,
    pub vertex: Option<Function>,
    pub fragment: Option<Function>,
}

/// A `@shader_value` record.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub name: Box<str>,
    pub fields: Vec<Binding>,
    pub span: Span,
}

impl AsRef<str> for Record {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

/// A named, typed interface member or record field.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub name: Box<str>,
    pub ty: Ty,
    pub span: Span,
}

/// Which pipeline stage an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Vertex,
    Fragment,
}

/// A value an entry receives from the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    /// `vertex_id: U32`, the vertex of the instance's quad.
    VertexId,
    /// `instance_id: U32`.
    InstanceId,
    /// `frag_coord: Vec4F32`, the fragment's window position.
    FragCoord,
}

impl Builtin {
    /// The builtin an entry parameter `name` of `stage` receives.
    pub fn of(stage: Stage, name: &str) -> Option<Builtin> {
        match (stage, name) {
            (Stage::Vertex, "vertex_id") => Some(Builtin::VertexId),
            (Stage::Vertex, "instance_id") => Some(Builtin::InstanceId),
            (Stage::Fragment, "frag_coord") => Some(Builtin::FragCoord),
            _ => None,
        }
    }

    /// The builtins `stage` offers, by parameter name.
    pub const fn names(stage: Stage) -> &'static [&'static str] {
        match stage {
            Stage::Vertex => &["vertex_id", "instance_id"],
            Stage::Fragment => &["frag_coord"],
        }
    }

    pub const fn ty(self) -> Ty {
        match self {
            Builtin::VertexId | Builtin::InstanceId => Ty::U32,
            Builtin::FragCoord => Ty::VEC4,
        }
    }
}

/// The fields of [`Ty::VertexOutput`].
pub const VERTEX_OUTPUT_FIELDS: &[(&str, Ty)] = &[("clip_position", Ty::VEC4)];

/// A shader `fn` or an entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: Box<str>,
    /// The entry's stage; `None` for a `fn`.
    pub stage: Option<Stage>,
    /// Its parameters are its first locals, this many.
    pub params: u32,
    /// What an entry's parameters receive, one per parameter.
    pub builtins: Vec<Builtin>,
    pub ret: Ty,
    /// Every local, the parameters first.
    pub locals: Vec<Local>,
    pub body: Block,
    pub span: Span,
}

/// A parameter or `let` binding.
#[derive(Debug, Clone, PartialEq)]
pub struct Local {
    pub name: Box<str>,
    pub ty: Ty,
    pub mutable: bool,
}

/// Statements run in order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Block(pub Vec<Stmt>);

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// Binds a local to a value.
    Let(u32, Expr),
    /// Binds a local to its type's zero value.
    Declare(u32),
    Assign(Place, Expr),
    If(Expr, Block, Block),
    For(Box<Loop>),
    Break,
    Continue,
    Return(Option<Expr>),
    /// Ends the fragment without writing it.
    Discard,
}

/// `for var in start..end` (or `..=`), running at most `max` times.
#[derive(Debug, Clone, PartialEq)]
pub struct Loop {
    /// The counter local, `I32` or `U32`.
    pub var: u32,
    pub start: Expr,
    pub end: Expr,
    pub inclusive: bool,
    /// The static bound on the iterations.
    pub max: u32,
    /// Whether the bound is a `@max_iterations` the range may exceed, so the
    /// loop counts its iterations; a literal range is its own bound.
    pub guarded: bool,
    pub body: Block,
}

/// What an assignment writes: a mutable local or a varying, or a field, lane
/// or column within it.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub root: Root,
    /// Record field indices, vector lanes and matrix columns, outermost first.
    pub path: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    Local(u32),
    Varying(u32),
}

/// A typed expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub ty: Ty,
    pub kind: ExprKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Bool(bool),
    I32(i32),
    U32(u32),
    F32(f32),
    Local(u32),
    Uniform(u32),
    Instance(u32),
    Varying(u32),
    Texture(u32),
    Sampler(u32),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// A vector, matrix, color or record of [`Expr::ty`] from its parts: the
    /// lanes of scalars and vectors filling a vector or color in order (one
    /// scalar fills every lane), a matrix's columns or its scalars by column,
    /// a record's fields in order.
    Construct(Vec<Expr>),
    /// Lanes of a vector or color, as many as [`Expr::ty`] has.
    Swizzle(Box<Expr>, [u8; 4]),
    /// A record's or `VertexOutput`'s field.
    Member(Box<Expr>, u32),
    /// A vector's lane or a matrix's column by a computed index, clamped to
    /// the last.
    Index(Box<Expr>, Box<Expr>),
    /// The program's function at this index.
    Call(u32, Vec<Expr>),
    Intrinsic(Intrinsic, Vec<Expr>),
    /// A numeric scalar or vector converted lane by lane to [`Expr::ty`].
    Convert(Box<Expr>),
}

impl Expr {
    pub fn new(ty: Ty, kind: ExprKind) -> Expr {
        Expr { ty, kind }
    }
}

impl Program {
    /// `ty` as source spells it.
    pub fn spell(&self, ty: Ty) -> String {
        ty.spelling(&self.records)
    }

    /// The fields of a record or `VertexOutput`, by name and type.
    pub fn fields(&self, ty: Ty) -> Option<Vec<(&str, Ty)>> {
        match ty {
            Ty::Record(i) => Some(
                self.records
                    .get(i as usize)?
                    .fields
                    .iter()
                    .map(|f| (&*f.name, f.ty))
                    .collect(),
            ),
            Ty::VertexOutput => Some(VERTEX_OUTPUT_FIELDS.to_vec()),
            _ => None,
        }
    }

    /// Whether `parts` construct a `ty`: see [`ExprKind::Construct`].
    pub fn constructs(&self, ty: Ty, parts: &[Ty]) -> bool {
        validate::constructs(self, ty, parts)
    }

    /// The entries it has, vertex first.
    pub fn entries(&self) -> impl Iterator<Item = &Function> {
        self.vertex.iter().chain(&self.fragment)
    }
}
