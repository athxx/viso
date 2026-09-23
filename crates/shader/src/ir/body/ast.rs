//! The typed body AST: what a built-in's vertex/fragment/helper fragments mean,
//! independent of how any one shading language spells them.
//!
//! The parser fills the syntax; the checker resolves every identifier to a
//! [`Res`] and fills every expression's [`Ty`]. The printers read only checked
//! trees.

use crate::ir::body::lex::Span;

/// A scalar element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    /// `bool`.
    Bool,
    /// 32-bit float.
    F32,
    /// 32-bit signed integer.
    I32,
    /// 32-bit unsigned integer.
    U32,
}

/// The attribute struct a vertex stage reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructName {
    /// The vertex → fragment varyings.
    VOut,
    /// Per-instance attributes.
    InstanceIn,
    /// Per-vertex attributes.
    VertexIn,
    /// The inline uniform block.
    Uniforms,
}

/// A type as written in source: a declaration, parameter, return, or
/// constructor type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeName {
    /// A scalar (`float`, `int`, `uint`, `bool`).
    Scalar(Scalar),
    /// A 2/3/4-lane vector (`float2`, `uint4`, …).
    Vector(Scalar, u8),
    /// One of the interface structs.
    Struct(StructName),
}

impl TypeName {
    /// The value type this names.
    pub fn ty(self) -> Ty {
        match self {
            TypeName::Scalar(s) => Ty::Scalar(s),
            TypeName::Vector(s, n) => Ty::Vector(s, n),
            TypeName::Struct(s) => Ty::Struct(s),
        }
    }

    /// Parse a source type keyword.
    pub fn from_keyword(word: &str) -> Option<TypeName> {
        match word {
            "VOut" => return Some(TypeName::Struct(StructName::VOut)),
            "InstanceIn" => return Some(TypeName::Struct(StructName::InstanceIn)),
            "VertexIn" => return Some(TypeName::Struct(StructName::VertexIn)),
            _ => {}
        }
        let (scalar, rest) = [
            ("float", Scalar::F32),
            ("uint", Scalar::U32),
            ("int", Scalar::I32),
            ("bool", Scalar::Bool),
        ]
        .into_iter()
        .find_map(|(k, s)| word.strip_prefix(k).map(|rest| (s, rest)))?;
        match rest {
            "" => Some(TypeName::Scalar(scalar)),
            "2" => Some(TypeName::Vector(scalar, 2)),
            "3" => Some(TypeName::Vector(scalar, 3)),
            "4" => Some(TypeName::Vector(scalar, 4)),
            _ => None,
        }
    }
}

/// The type of a checked expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    /// Not yet checked.
    Unknown,
    /// A concrete scalar.
    Scalar(Scalar),
    /// A concrete vector.
    Vector(Scalar, u8),
    /// An integer literal that takes the type of its context (`i32`, `u32` or
    /// `f32`), resolving to `i32` when nothing constrains it.
    AbsInt,
    /// An interface struct value.
    Struct(StructName),
    /// A 2D float texture.
    Texture,
    /// A sampler.
    Sampler,
    /// The attribute buffer, only ever indexed.
    AttrBuffer,
    /// No value (a helper called as a statement).
    Void,
}

impl Ty {
    /// The element scalar of a scalar or vector.
    pub fn scalar(self) -> Option<Scalar> {
        match self {
            Ty::Scalar(s) | Ty::Vector(s, _) => Some(s),
            _ => None,
        }
    }

    /// Lane count: 1 for a scalar, `n` for a vector.
    pub fn lanes(self) -> Option<u8> {
        match self {
            Ty::Scalar(_) | Ty::AbsInt => Some(1),
            Ty::Vector(_, n) => Some(n),
            _ => None,
        }
    }

    /// Whether this is a vector.
    pub fn is_vector(self) -> bool {
        matches!(self, Ty::Vector(..))
    }
}

/// A shader-stage global an identifier may resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Global {
    /// `vid` — the vertex index.
    Vid,
    /// `iid` — the instance index.
    Iid,
    /// `instances` — the per-instance attribute buffer.
    Instances,
    /// `verts` — the per-vertex attribute buffer.
    Verts,
    /// `u` — the uniform block.
    Uniforms,
    /// `in` — the fragment's interpolated varyings.
    In,
    /// `tex` — texture slot 0.
    Tex,
    /// `dst_tex` — texture slot 1.
    DstTex,
    /// `samp` — the shared sampler.
    Samp,
    /// `M_PI_F`.
    Pi,
}

/// What an identifier refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Res {
    /// Not yet resolved.
    Unresolved,
    /// A parameter or local of the enclosing function, by index into
    /// [`Function::locals`].
    Local(u32),
    /// A stage global.
    Global(Global),
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `-`
    Neg,
    /// `!`
    Not,
    /// `~`
    BitNot,
}

impl UnOp {
    /// Source spelling.
    pub fn text(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Not => "!",
            UnOp::BitNot => "~",
        }
    }
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Mul,
    Div,
    Rem,
    Add,
    Sub,
    Shl,
    Shr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    BitAnd,
    BitXor,
    BitOr,
    And,
    Or,
}

impl BinOp {
    /// Source spelling.
    pub fn text(self) -> &'static str {
        match self {
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::BitAnd => "&",
            BinOp::BitXor => "^",
            BinOp::BitOr => "|",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }

    /// C binding strength: higher binds tighter.
    pub fn precedence(self) -> u8 {
        match self {
            BinOp::Mul | BinOp::Div | BinOp::Rem => 10,
            BinOp::Add | BinOp::Sub => 9,
            BinOp::Shl | BinOp::Shr => 8,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 7,
            BinOp::Eq | BinOp::Ne => 6,
            BinOp::BitAnd => 5,
            BinOp::BitXor => 4,
            BinOp::BitOr => 3,
            BinOp::And => 2,
            BinOp::Or => 1,
        }
    }

    /// The operator a source punctuation token spells, if any.
    pub fn from_punct(p: &str) -> Option<BinOp> {
        Some(match p {
            "*" => BinOp::Mul,
            "/" => BinOp::Div,
            "%" => BinOp::Rem,
            "+" => BinOp::Add,
            "-" => BinOp::Sub,
            "<<" => BinOp::Shl,
            ">>" => BinOp::Shr,
            "<" => BinOp::Lt,
            "<=" => BinOp::Le,
            ">" => BinOp::Gt,
            ">=" => BinOp::Ge,
            "==" => BinOp::Eq,
            "!=" => BinOp::Ne,
            "&" => BinOp::BitAnd,
            "^" => BinOp::BitXor,
            "|" => BinOp::BitOr,
            "&&" => BinOp::And,
            "||" => BinOp::Or,
            _ => return None,
        })
    }

    /// Arithmetic operators that accept a scalar/vector mix.
    pub fn is_arithmetic(self) -> bool {
        matches!(
            self,
            BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Add | BinOp::Sub
        )
    }

    /// Integer-only operators.
    pub fn is_bitwise(self) -> bool {
        matches!(
            self,
            BinOp::Shl | BinOp::Shr | BinOp::BitAnd | BinOp::BitXor | BinOp::BitOr
        )
    }

    /// Operators producing `bool` from two comparable operands.
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne
        )
    }
}

/// A plain or compound assignment operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    /// `=`
    Set,
    /// `op=`
    Compound(BinOp),
}

impl AssignOp {
    /// Source spelling.
    pub fn text(self) -> &'static str {
        match self {
            AssignOp::Set => "=",
            AssignOp::Compound(BinOp::Add) => "+=",
            AssignOp::Compound(BinOp::Sub) => "-=",
            AssignOp::Compound(BinOp::Mul) => "*=",
            AssignOp::Compound(BinOp::Div) => "/=",
            AssignOp::Compound(BinOp::Rem) => "%=",
            AssignOp::Compound(BinOp::BitAnd) => "&=",
            AssignOp::Compound(BinOp::BitOr) => "|=",
            AssignOp::Compound(BinOp::BitXor) => "^=",
            AssignOp::Compound(BinOp::Shl) => "<<=",
            AssignOp::Compound(BinOp::Shr) => ">>=",
            AssignOp::Compound(_) => unreachable!("the parser only builds spellable compounds"),
        }
    }

    /// The operator a source punctuation token spells, if any.
    pub fn from_punct(p: &str) -> Option<AssignOp> {
        Some(match p {
            "=" => AssignOp::Set,
            "+=" => AssignOp::Compound(BinOp::Add),
            "-=" => AssignOp::Compound(BinOp::Sub),
            "*=" => AssignOp::Compound(BinOp::Mul),
            "/=" => AssignOp::Compound(BinOp::Div),
            "%=" => AssignOp::Compound(BinOp::Rem),
            "&=" => AssignOp::Compound(BinOp::BitAnd),
            "|=" => AssignOp::Compound(BinOp::BitOr),
            "^=" => AssignOp::Compound(BinOp::BitXor),
            "<<=" => AssignOp::Compound(BinOp::Shl),
            ">>=" => AssignOp::Compound(BinOp::Shr),
            _ => return None,
        })
    }
}

/// An expression with its location and (once checked) its type.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// What it is.
    pub kind: ExprKind,
    /// Where it is.
    pub span: Span,
    /// Its type; [`Ty::Unknown`] until checked.
    pub ty: Ty,
}

/// The shape of an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A name.
    Var { name: String, res: Res },
    /// A float literal, verbatim.
    Float(String),
    /// A signed integer literal, verbatim.
    Int(String),
    /// An unsigned integer literal with its `u` suffix, verbatim.
    Uint(String),
    /// `(e)` — kept so every printer reproduces the source grouping.
    Paren(Box<Expr>),
    /// `op e`.
    Unary(UnOp, Box<Expr>),
    /// `l op r`.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `c ? t : f`.
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A builtin or helper call.
    Call { name: String, args: Vec<Expr> },
    /// A scalar conversion or vector constructor, `float2(...)`.
    Construct { ty: TypeName, args: Vec<Expr> },
    /// `base.name` — a struct member or a vector swizzle.
    Field { base: Box<Expr>, name: String },
    /// `base.method(args)` — a texture query or sample.
    Method {
        base: Box<Expr>,
        method: String,
        args: Vec<Expr>,
    },
    /// `base[index]`.
    Index { base: Box<Expr>, index: Box<Expr> },
}

/// A statement with its location.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    /// What it is.
    pub kind: StmtKind,
    /// Where it starts.
    pub span: Span,
    /// A blank line separates it from the previous statement.
    pub blank_before: bool,
}

/// The shape of a statement.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// A `//` comment line.
    Comment(String),
    /// `T name;` or `T name = init;`.
    Decl {
        ty: TypeName,
        name: String,
        local: u32,
        init: Option<Expr>,
    },
    /// `target op value;`.
    Assign {
        target: Expr,
        op: AssignOp,
        value: Expr,
    },
    /// `++target;` / `--target;` (prefix or postfix).
    Step {
        target: Expr,
        decrement: bool,
        prefix: bool,
    },
    /// `if (cond) { ... } else ...`.
    If {
        cond: Expr,
        then: Vec<Stmt>,
        els: Option<Else>,
    },
    /// `switch (selector) { case ...: ... default: ... }`.
    Switch { selector: Expr, cases: Vec<Case> },
    /// `for (init; cond; step) { ... }`.
    For {
        init: Box<Stmt>,
        cond: Expr,
        step: Box<Stmt>,
        body: Vec<Stmt>,
    },
    /// `return;` / `return e;`.
    Return(Option<Expr>),
    /// `break;`.
    Break,
}

/// The `else` arm of an `if`.
#[derive(Debug, Clone, PartialEq)]
pub enum Else {
    /// `else { ... }`.
    Block(Vec<Stmt>),
    /// `else if ...`.
    If(Box<Stmt>),
}

/// One `case`/`default` arm. Every arm ends in `break` or `return`.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    /// The label, or `None` for `default`.
    pub label: Option<Expr>,
    /// The arm's statements.
    pub body: Vec<Stmt>,
}

/// A parameter or local variable of a function.
#[derive(Debug, Clone, PartialEq)]
pub struct Local {
    /// Source name.
    pub name: String,
    /// Declared type.
    pub ty: TypeName,
    /// A parameter (the first `params` locals are the parameters, in order).
    pub is_param: bool,
    /// Written after its declaration (assignment, compound assignment,
    /// increment, or a member/swizzle store).
    pub mutated: bool,
}

/// A helper function, or an entry point's body wrapped as one.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// Return type.
    pub ret: TypeName,
    /// Source name.
    pub name: String,
    /// Parameter count; parameters are `locals[..params]`.
    pub params: usize,
    /// Every parameter and local, indexed by [`Res::Local`].
    pub locals: Vec<Local>,
    /// The statements.
    pub body: Vec<Stmt>,
    /// Where the function starts.
    pub span: Span,
}

/// A top-level item of a helper fragment.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A `//` comment line.
    Comment { text: String, blank_before: bool },
    /// A `static inline` helper.
    Function { func: Function, blank_before: bool },
}
