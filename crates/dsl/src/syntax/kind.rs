//! [`SyntaxKind`] — the single flat tag for every token *and* every CST node.
//!
//! One `#[repr(u16)]` enum keeps the green tree's per-element tag a `Copy` two
//! bytes (no boxing, no string), so a lossless CST node is compact even though
//! it carries far more distinct kinds than a coarse highlighter would. The
//! ordering groups trivia, then literals, then identifiers, then keywords, then
//! punctuation, then the handful of CST node kinds the builder needs now — the
//! grammar in the next slice appends the rest of the node kinds at the end.
//!
//! Context words (spec section 12.7 — `viso`, `empty`, prelude types like `Bool`, unit
//! suffixes) are deliberately **not** keywords: they lex as [`SyntaxKind::Ident`]
//! and only gain meaning at parse/resolve time. Keyword case matters: `state` is
//! a keyword, `State` is an ordinary identifier.

/// The kind of a lexed token or a CST node.
///
/// Values are stable within a build but not across builds; never serialize the
/// numeric value. Use the named variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
#[non_exhaustive]
pub enum SyntaxKind {
    // --- Trivia (whitespace + comments). Preserved losslessly in the CST. ---
    /// A run of spaces / tabs / newlines (`U+0020 U+0009 U+000A U+000D`).
    Whitespace,
    /// `// ...` to end of line.
    LineComment,
    /// `/* ... */`, nestable.
    BlockComment,
    /// `/// ...` declaration doc comment.
    DocComment,
    /// `//! ...` module doc comment.
    ModuleDocComment,

    // --- Literals ---
    /// Decimal / `0x` / `0o` / `0b` integer, optional typed suffix (spec section 15).
    IntLiteral,
    /// Decimal float with a mandatory fraction or exponent (spec section 16).
    FloatLiteral,
    /// A numeric body immediately followed by a unit suffix or `%` (spec section 19).
    UnitLiteral,
    /// `"..."` string with escapes (spec section 17).
    StringLiteral,
    /// `r"..."` / `r#"..."#` raw string (spec section 17).
    RawStringLiteral,
    /// `'c'` character literal (spec section 17).
    CharLiteral,
    /// `#RGB` / `#RGBA` / `#RRGGBB` / `#RRGGBBAA` color literal (spec section 18).
    ColorLiteral,

    // --- Names ---
    /// An ordinary identifier (also every context word and `true`/`false`/`None`
    /// are *keywords*, but prelude type names, `viso`, `empty` lex here).
    Ident,
    /// `r#name` raw identifier (spec section 11); decodes to an ordinary symbol name.
    RawIdent,

    // --- Declaration keywords (spec section 12.1) ---
    ImportKw,
    ExportKw,
    AsKw,
    ComponentKw,
    SystemKw,
    RecordKw,
    EnumKw,
    TraitKw,
    ImplKw,
    TypeKw,
    ImplementsKw,
    WhereKw,
    ForKw,
    InputKw,
    StateKw,
    ComputedKw,
    EventKw,
    SlotKw,
    ConstKw,
    FnKw,
    ActionKw,
    EffectKw,
    TaskKw,
    ResourceKw,
    ViewKw,
    StyleKw,
    ThemeKw,
    TemplateKw,
    PartKw,
    NativeKw,
    RequiresKw,
    CapabilityKw,

    // --- Control-flow / behavior keywords (spec section 12.2) ---
    LetKw,
    MutKw,
    ReturnKw,
    BreakKw,
    ContinueKw,
    IfKw,
    ElseKw,
    MatchKw,
    WhileKw,
    LoopKw,
    InKw,
    OnKw,
    CaptureKw,
    BubbleKw,
    EmitKw,
    TransactionKw,
    StartKw,
    AwaitKw,
    MoveKw,
    WhenKw,
    RunKw,
    CleanupKw,
    SuccessKw,
    ErrorKw,
    CancelledKw,

    // --- View / resource keywords (spec section 12.3) ---
    NodeKw,
    FillKw,
    BindKw,
    UsingKw,
    UseKw,
    OverrideKw,
    ReplaceKw,
    PreserveKw,
    KeyKw,
    LoadKw,
    PolicyKw,
    ScopeKw,

    // --- Shader keywords (spec section 12.4) ---
    ShaderKw,
    VertexKw,
    FragmentKw,
    ComputeKw,
    UniformKw,
    InstanceKw,
    VaryingKw,
    TextureKw,
    SamplerKw,

    // --- Type / literal keywords (spec section 12.5) ---
    TrueKw,
    FalseKw,
    NoneKw,
    SelfValueKw,
    SelfTypeKw,
    DynKw,

    // --- Reserved-but-forbidden words (spec section 12.6). Lexed as their own kind so
    // the parser can emit a precise "reserved word" diagnostic rather than
    // silently treating them as identifiers. ---
    ChildKw,
    StoreKw,
    MergeKw,
    ExtendKw,
    InheritKw,
    ClassKw,
    MacroKw,
    UnsafeKw,
    ExternKw,
    StaticKw,
    YieldKw,
    TryKw,

    // --- Delimiters (spec section 13) ---
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `,`
    Comma,
    /// `;`
    Semi,
    /// `:`
    Colon,
    /// `::`
    ColonColon,
    /// `@`
    At,

    // --- Operators (spec section 13) ---
    /// `=`
    Eq,
    /// `+=`
    PlusEq,
    /// `-=`
    MinusEq,
    /// `*=`
    StarEq,
    /// `/=`
    SlashEq,
    /// `%=`
    PercentEq,
    /// `&=`
    AmpEq,
    /// `|=`
    PipeEq,
    /// `^=`
    CaretEq,
    /// `<<=`
    ShlEq,
    /// `>>=`
    ShrEq,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `!`
    Bang,
    /// `~`
    Tilde,
    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// `^`
    Caret,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `==`
    EqEq,
    /// `!=`
    Neq,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// `??`
    QuestionQuestion,
    /// `.`
    Dot,
    /// `?.`
    QuestionDot,
    /// `?`
    Question,
    /// `..`
    DotDot,
    /// `..=`
    DotDotEq,
    /// `->`
    Arrow,
    /// `=>`
    FatArrow,
    /// `<=>`
    BidiArrow,

    // --- Sentinels ---
    /// End of input. Always the final token of a lex.
    Eof,
    /// A byte / sequence the lexer could not classify. Carries a `LexError`.
    Error,

    // --- CST node kinds (not produced by the lexer; built by the tree builder
    // / parser). Slice K needs only the root and an error wrapper; the grammar
    // slice appends its production node kinds after these. ---
    /// The document root node.
    Root,
    /// A node wrapping tokens the parser could not fit into a valid production;
    /// keeps the tree lossless while marking the span as recovered.
    ErrorNode,
    /// A zero-width placeholder for a token the grammar required but the source
    /// omitted, so a following node still has the child slot it expects.
    MissingToken,
    /// A coarse item grouped by the minimal skeleton parser: everything from a
    /// declaration keyword up to and including its terminating `;` or `{...}`.
    Item,
    /// A `{ ... }`-delimited block, children between the braces preserved.
    Block,

    // --- Grammar node kinds (Appendix A). The typed parser produces these; the
    // coarse `Item` is superseded by the specific declaration nodes below. New
    // kinds are appended at the end so earlier discriminants stay put within a
    // build (the value is never serialized). ---
    /// The `.vs` file entry: `ImportDecl* TopLevelDecl* EOF` (A.2).
    CompilationUnit,
    /// The `ui!` entry: `ViewStructureItem* EOF` (A.2) — a bare view fragment.
    ViewFragment,
    /// The `component!` entry: `ImportDecl* ComponentDecl EOF` (A.2).
    ComponentEntry,

    // Declarations.
    /// `import ModulePath ImportSuffix? ";"` (A.2).
    ImportDecl,
    /// A `::`-separated module path `IDENT ("::" IDENT)*` (A.2).
    ModulePath,
    /// One `{ a, b as c }` selective-import item (A.2).
    ImportItem,
    /// The renamed-import tail `"as" IDENT` on an import or item (A.2).
    RenameClause,
    /// An `export`-prefixed top-level declaration; visibility travels with the
    /// wrapped declaration (A.2).
    ExportDecl,
    /// A leading `@Path(args)` attribute (A.2).
    Attribute,
    /// The parenthesized argument list of an attribute (A.2).
    AttributeArgs,
    /// `record IDENT ... "{" RecordField* "}"` (A.4).
    RecordDecl,
    /// `Attribute* IDENT ":" Type ("=" Expr)? ";"` inside a record (A.4).
    RecordField,
    /// `enum IDENT ... "{" EnumVariant* "}"` (A.4).
    EnumDecl,
    /// `Attribute* IDENT VariantPayload? ";"` inside an enum (A.4).
    EnumVariant,
    /// A tuple or record payload attached to an enum variant (A.4).
    VariantPayload,
    /// `const IDENT ":" Type "=" Expr ";"` (A.4).
    ConstDecl,
    /// `type IDENT GenericParams? "=" Type ";"` (A.4).
    TypeAliasDecl,
    /// `component IDENT ... "{" ComponentMember* "}"` (A.6).
    ComponentDecl,
    /// `system IDENT ... "{" ... "}"` — a scheduler system declaration.
    SystemDecl,
    /// `fn IDENT GenericParams? "(" ParamList ")" ReturnType? ... Block` (A.5).
    FnDecl,
    /// `action IDENT "(" ParamList ")" ReturnType? Block` (A.5).
    ActionDecl,
    /// `task IDENT "(" ParamList ")" ReturnType? Block` (A.5).
    TaskDecl,
    /// `input IDENT ":" Type ("=" Expr)? ";"` (A.6).
    InputDecl,
    /// `state IDENT (":" Type)? "=" Expr ";"` (A.6) — `=` is initialization.
    StateDecl,
    /// `computed IDENT (":" Type)? "=" Expr ";"` (A.6).
    ComputedDecl,
    /// `event IDENT "(" EventParam* ")" ";"` (A.6).
    EventDecl,
    /// One `IDENT ":" Type` inside an event parameter list (A.6).
    EventParam,
    /// `slot IDENT ":" Type ("=" SlotDefault)? ";"` (A.6).
    SlotDecl,

    // Generics / parameters / types.
    /// A `< ... >` list of generic parameters on a declaration (A.4).
    GenericParams,
    /// One generic parameter (a type or const parameter) (A.4).
    GenericParam,
    /// An `implements Trait (+ Trait)*` clause (A.4).
    ImplementsClause,
    /// A `where` clause (A.4).
    WhereClause,
    /// A `requires { ... }` capability clause (A.5).
    CapabilityClause,
    /// A `"(" Parameter,* ")"` parameter list (A.5).
    ParamList,
    /// One `"mut"? IDENT ":" Type ("=" Expr)?` parameter (A.5).
    Param,
    /// A `"->" Type` return type (A.5).
    ReturnType,
    /// A `TypePathSegment ("::" TypePathSegment)*` type path (A.3).
    TypePath,
    /// One `IDENT GenericArgs?` segment of a type path (A.3).
    TypePathSegment,
    /// A `< GenericArg,* >` list of generic arguments (A.3).
    GenericArgs,
    /// A tuple type `"(" Type,* ")"` (A.3).
    TupleType,
    /// An array or slice type `"[" Type (";" Expr)? "]"` (A.3).
    ArrayType,

    // View.
    /// `view ViewBlock` (A.8).
    ViewDecl,
    /// A `"{" ViewStructureItem* "}"` view block (A.8).
    ViewBlock,
    /// `node IDENT ":" ComponentType NodeBody` (A.8).
    NamedNode,
    /// `ComponentType NodeBody` — an anonymous child node (A.8).
    AnonymousNode,
    /// `part IDENT ":" ComponentType NodeBody` (A.8).
    PartNode,
    /// A `"{" NodeMember* "}"` node body (A.8).
    NodeBody,
    /// `PropertyPath ":" Expr ";"` — a property binding (A.8).
    PropertyBinding,
    /// An `IDENT ("." IDENT)*` property path (A.8).
    PropertyPath,
    /// `bind PropertyPath "<=>" AssignablePath ("using" TypePath)? ";"` (A.8).
    TwoWayBinding,
    /// `on EventPhase? IDENT ("(" Pattern ")")? Block` (A.8).
    EventHandler,
    /// `fill IDENT ViewBlock` (A.8).
    FillClause,
    /// `if HeadExpr ("preserve" STRING)? ViewBlock ("else" ...)?` (A.8).
    ViewIf,
    /// `for Pattern "in" HeadExpr "key" HeadExpr ViewBlock` (A.8).
    ViewFor,
    /// `match HeadExpr "{" ViewMatchArm,* "}"` (A.8).
    ViewMatch,
    /// `Pattern ("if" Expr)? "=>" ViewBlock` inside a view match (A.8).
    ViewMatchArm,

    // Statements.
    /// `let "mut"? Pattern (":" Type)? "=" Expr ";"` (A.11).
    LetStmt,
    /// `AssignablePath AssignmentOperator Expr ";"` (A.11).
    AssignStmt,
    /// An `AssignablePath` (the left side of an assignment) (A.11).
    AssignablePath,
    /// `Expr ";"` — an expression statement (A.11).
    ExprStmt,
    /// `return Expr? ";"` (A.11).
    ReturnStmt,
    /// `break Expr? ";"` (A.11).
    BreakStmt,
    /// `continue ";"` (A.11).
    ContinueStmt,
    /// `emit IDENT "(" ArgumentList ")" ";"` (A.11).
    EmitStmt,
    /// `transaction Block` (A.11).
    TransactionStmt,
    /// `while HeadExpr Block` (A.11).
    WhileStmt,
    /// `for Pattern "in" HeadExpr Block` — a behavior loop (no key) (A.11).
    ForStmt,
    /// `loop Block` (A.11).
    LoopStmt,
    /// `if HeadExpr Block ("else" (IfStmt | Block))?` — statement form (A.11).
    IfStmt,
    /// A `match` used in statement position (A.11).
    MatchStmt,

    // Expressions.
    /// A literal expression (int/float/string/color/bool/None/…) (A.12).
    LiteralExpr,
    /// A path expression `Path` / `self` / `Self` (A.12).
    PathExpr,
    /// A binary operator expression `lhs op rhs` (A.12).
    BinaryExpr,
    /// A prefix unary expression `op expr` (A.12).
    UnaryExpr,
    /// A `expr "as" Type` cast (A.12).
    CastExpr,
    /// A range expression `lo (".." | "..=") hi` (A.12).
    RangeExpr,
    /// A call `callee GenericCallArgs? "(" ArgumentList ")"` (A.12).
    CallExpr,
    /// An index `expr "[" Expr "]"` (A.12).
    IndexExpr,
    /// A field access `expr "." IDENT` (A.12).
    FieldExpr,
    /// An optional field access `expr "?." IDENT` (A.12).
    OptionalFieldExpr,
    /// A try `expr "?"` (A.12).
    TryExpr,
    /// A turbofish `"::" GenericArgs` on a call (A.12).
    GenericCallArgs,
    /// A `"(" Expr,* ")"` — parenthesized expr or tuple (A.12).
    ParenExpr,
    /// A tuple expression `"(" Expr "," ... ")"` (A.12).
    TupleExpr,
    /// A list expression `"[" Expr,* "]"` (A.12).
    ListExpr,
    /// A record expression `Path GenericCallArgs? "{" RecordInitializer,* "}"` (A.12).
    RecordExpr,
    /// One `IDENT (":" Expr)?` or `".." Expr` record initializer (A.12).
    RecordExprField,
    /// An `if ... else ...` expression (both arms required) (A.12).
    IfExpr,
    /// A `match` expression (A.12).
    MatchExpr,
    /// `Pattern ("if" Expr)? "=>" (Expr | Block)` in a match expression (A.12).
    MatchArm,
    /// A closure `"move"? (ClosureParams | "||") ... (Expr | Block)` (A.12).
    ClosureExpr,
    /// A `"|" ClosureParameter,* "|"` closure parameter list (A.12).
    ClosureParams,
    /// One `"mut"? Pattern (":" Type)?` closure parameter (A.12).
    ClosureParam,
    /// An `IDENT ":" Expr` or positional `Expr` call argument (A.12).
    Argument,
    /// The argument list inside a call `"(" Argument,* ")"` (A.12).
    ArgumentList,

    // Patterns.
    /// A top-level pattern; wraps the specific pattern shapes below (A.13).
    Pattern,
    /// An `a | b | c` or-pattern (A.13).
    OrPattern,
    /// An `IDENT "@" Pattern` binding pattern (A.13).
    BindingPattern,
    /// A `lo (".." | "..=") hi` range pattern (A.13).
    RangePattern,
    /// The wildcard `_` pattern (A.13).
    WildcardPattern,
    /// A literal pattern (A.13).
    LiteralPattern,
    /// An `"mut"? IDENT` identifier (binding) pattern (A.13).
    IdentPattern,
    /// A `"(" Pattern,* ")"` tuple pattern (A.13).
    TuplePattern,
    /// A `"[" ListPatternItem,* "]"` list pattern (A.13).
    ListPattern,
    /// A `TypePath (…)`/`{…}` constructor pattern (A.13).
    ConstructorPattern,
    /// A `IDENT "::" IDENT (…)` qualified-variant pattern (A.13).
    QualifiedVariantPattern,
    /// One `IDENT (":" Pattern)?` / `".."` field in a record constructor pattern (A.13).
    RecordPatternField,

    // Advanced (parsed, not resolved this slice).
    /// A declaration in the Advanced tier (`trait`/`impl`/`template`/`style`/
    /// `theme`/`shader`/`native`) parsed to a placeholder wrapper: its interior
    /// is grouped losslessly but it gets no name resolution yet.
    AdvancedItem,
}

impl SyntaxKind {
    /// Whether this kind is trivia (whitespace or any comment). Trivia are kept
    /// in the flat token stream and the green tree but are skipped by the parser
    /// when matching grammar.
    #[inline]
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace
                | Self::LineComment
                | Self::BlockComment
                | Self::DocComment
                | Self::ModuleDocComment
        )
    }

    /// Whether this kind is one of the closed keyword set (spec section 12.1–12.6).
    /// Context words (section 12.7) are `Ident`, so they return `false`.
    #[inline]
    pub fn is_keyword(self) -> bool {
        // Keywords occupy one contiguous run in the enum: from the first
        // declaration keyword through the last reserved word.
        (Self::ImportKw as u16..=Self::TryKw as u16).contains(&(self as u16))
    }

    /// Whether this kind is a CST-only node kind (never emitted by the lexer).
    ///
    /// The node kinds occupy the tail of the enum: the coarse skeleton set
    /// (`Root`..=`Block`) followed by the grammar node kinds
    /// (`CompilationUnit`..=`AdvancedItem`). Everything below `CompilationUnit`
    /// is a token kind. A single lower-bound comparison covers the whole tail
    /// because no token kind is appended after the nodes.
    #[inline]
    pub fn is_node(self) -> bool {
        self as u16 >= Self::Root as u16
    }

    /// Whether this kind is one of the declaration keywords the skeleton parser
    /// treats as an item boundary and a recovery sync point (spec section 12.1).
    #[inline]
    pub fn is_item_start(self) -> bool {
        matches!(
            self,
            Self::ImportKw
                | Self::ExportKw
                | Self::ComponentKw
                | Self::SystemKw
                | Self::RecordKw
                | Self::EnumKw
                | Self::TraitKw
                | Self::ImplKw
                | Self::TypeKw
                | Self::InputKw
                | Self::StateKw
                | Self::ComputedKw
                | Self::EventKw
                | Self::SlotKw
                | Self::ConstKw
                | Self::FnKw
                | Self::ActionKw
                | Self::EffectKw
                | Self::TaskKw
                | Self::ResourceKw
                | Self::ViewKw
                | Self::StyleKw
                | Self::ThemeKw
                | Self::TemplateKw
                | Self::PartKw
                | Self::NativeKw
                | Self::ShaderKw
        )
    }

    /// Classify an identifier's text as its keyword kind, or [`Self::Ident`] if
    /// it is not a keyword. A closed `match` on the byte string — no map, no
    /// allocation — because the keyword set is small and fixed (spec section 12).
    ///
    /// `text` must be the exact source spelling of a normal identifier (not a
    /// raw identifier: `r#state` is always [`Self::RawIdent`], never a keyword).
    #[inline]
    pub fn from_ident(text: &str) -> SyntaxKind {
        match text {
            "import" => Self::ImportKw,
            "export" => Self::ExportKw,
            "as" => Self::AsKw,
            "component" => Self::ComponentKw,
            "system" => Self::SystemKw,
            "record" => Self::RecordKw,
            "enum" => Self::EnumKw,
            "trait" => Self::TraitKw,
            "impl" => Self::ImplKw,
            "type" => Self::TypeKw,
            "implements" => Self::ImplementsKw,
            "where" => Self::WhereKw,
            "for" => Self::ForKw,
            "input" => Self::InputKw,
            "state" => Self::StateKw,
            "computed" => Self::ComputedKw,
            "event" => Self::EventKw,
            "slot" => Self::SlotKw,
            "const" => Self::ConstKw,
            "fn" => Self::FnKw,
            "action" => Self::ActionKw,
            "effect" => Self::EffectKw,
            "task" => Self::TaskKw,
            "resource" => Self::ResourceKw,
            "view" => Self::ViewKw,
            "style" => Self::StyleKw,
            "theme" => Self::ThemeKw,
            "template" => Self::TemplateKw,
            "part" => Self::PartKw,
            "native" => Self::NativeKw,
            "requires" => Self::RequiresKw,
            "capability" => Self::CapabilityKw,

            "let" => Self::LetKw,
            "mut" => Self::MutKw,
            "return" => Self::ReturnKw,
            "break" => Self::BreakKw,
            "continue" => Self::ContinueKw,
            "if" => Self::IfKw,
            "else" => Self::ElseKw,
            "match" => Self::MatchKw,
            "while" => Self::WhileKw,
            "loop" => Self::LoopKw,
            "in" => Self::InKw,
            "on" => Self::OnKw,
            "capture" => Self::CaptureKw,
            "bubble" => Self::BubbleKw,
            "emit" => Self::EmitKw,
            "transaction" => Self::TransactionKw,
            "start" => Self::StartKw,
            "await" => Self::AwaitKw,
            "move" => Self::MoveKw,
            "when" => Self::WhenKw,
            "run" => Self::RunKw,
            "cleanup" => Self::CleanupKw,
            "success" => Self::SuccessKw,
            "error" => Self::ErrorKw,
            "cancelled" => Self::CancelledKw,

            "node" => Self::NodeKw,
            "fill" => Self::FillKw,
            "bind" => Self::BindKw,
            "using" => Self::UsingKw,
            "use" => Self::UseKw,
            "override" => Self::OverrideKw,
            "replace" => Self::ReplaceKw,
            "preserve" => Self::PreserveKw,
            "key" => Self::KeyKw,
            "load" => Self::LoadKw,
            "policy" => Self::PolicyKw,
            "scope" => Self::ScopeKw,

            "shader" => Self::ShaderKw,
            "vertex" => Self::VertexKw,
            "fragment" => Self::FragmentKw,
            "compute" => Self::ComputeKw,
            "uniform" => Self::UniformKw,
            "instance" => Self::InstanceKw,
            "varying" => Self::VaryingKw,
            "texture" => Self::TextureKw,
            "sampler" => Self::SamplerKw,

            "true" => Self::TrueKw,
            "false" => Self::FalseKw,
            "None" => Self::NoneKw,
            "self" => Self::SelfValueKw,
            "Self" => Self::SelfTypeKw,
            "dyn" => Self::DynKw,

            "child" => Self::ChildKw,
            "store" => Self::StoreKw,
            "merge" => Self::MergeKw,
            "extend" => Self::ExtendKw,
            "inherit" => Self::InheritKw,
            "class" => Self::ClassKw,
            "macro" => Self::MacroKw,
            "unsafe" => Self::UnsafeKw,
            "extern" => Self::ExternKw,
            "static" => Self::StaticKw,
            "yield" => Self::YieldKw,
            "try" => Self::TryKw,

            _ => Self::Ident,
        }
    }
}
