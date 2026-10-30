//! [`SyntaxKind`] — the single flat tag for every token *and* every CST node.
//!
//! One `#[repr(u16)]` enum keeps the green tree's per-element tag a `Copy` two
//! bytes (no boxing, no string), so a lossless CST node is compact even though
//! it carries far more distinct kinds than a coarse highlighter would. The
//! ordering groups trivia, then literals, then identifiers, then keywords, then
//! punctuation, then the handful of CST node kinds the tree builder needs, then
//! the grammar's node kinds.
//!
//! Context words (`viso`, `empty`, prelude types like `Bool`, unit suffixes) are
//! deliberately **not** keywords: they lex as [`SyntaxKind::Ident`]
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
    /// Decimal / `0x` / `0o` / `0b` integer, optional typed suffix.
    IntLiteral,
    /// Decimal float with a mandatory fraction or exponent.
    FloatLiteral,
    /// A numeric body immediately followed by a unit suffix or `%`.
    UnitLiteral,
    /// `"..."` string with escapes and no holes.
    StringLiteral,
    /// The start of a string with holes, up to and including the first hole's `{`.
    StringStart,
    /// A string's text between two holes, from a hole's `}` to the next's `{`.
    StringMiddle,
    /// The end of a string with holes, from the last hole's `}` to the `"`.
    StringEnd,
    /// `r"..."` / `r#"..."#` raw string.
    RawStringLiteral,
    /// `'c'` character literal.
    CharLiteral,
    /// `#RGB` / `#RGBA` / `#RRGGBB` / `#RRGGBBAA` color literal.
    ColorLiteral,

    // --- Names ---
    /// An ordinary identifier (also every context word and `true`/`false`/`None`
    /// are *keywords*, but prelude type names, `viso`, `empty` lex here).
    Ident,
    /// `r#name` raw identifier; decodes to an ordinary symbol name.
    RawIdent,
    /// An identifier naming a package module ahead of `::` in a path
    /// (`model::twice`'s `model`). The lexer never produces one: resolution
    /// re-tags an [`Self::Ident`] it binds to a whole-module import, so a
    /// path's segments are the declaration's name and what follows it.
    ModuleQualifier,

    // --- Declaration keywords ---
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

    // --- Control-flow / behavior keywords ---
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

    // --- View / resource keywords ---
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
    EmptyKw,

    // --- Shader keywords ---
    ShaderKw,
    VertexKw,
    FragmentKw,
    ComputeKw,
    UniformKw,
    InstanceKw,
    VaryingKw,
    TextureKw,
    SamplerKw,

    // --- Type / literal keywords ---
    TrueKw,
    FalseKw,
    NoneKw,
    SelfValueKw,
    SelfTypeKw,
    DynKw,

    // --- Reserved-but-forbidden words. Lexed as their own kind so
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

    // --- Delimiters ---
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

    // --- Operators ---
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
    // / parser). The flat tree needs only the root and an error wrapper; the
    // grammar's production node kinds follow these. ---
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

    // --- Grammar node kinds. The typed parser produces these; the
    // coarse `Item` is superseded by the specific declaration nodes below. New
    // kinds are appended at the end so earlier discriminants stay put within a
    // build (the value is never serialized). ---
    /// The `.vs` file entry: `ImportDecl* TopLevelDecl* EOF`.
    CompilationUnit,
    /// The `ui!` entry: `ViewStructureItem* EOF` — a bare view fragment.
    ViewFragment,
    /// The `component!` entry: `ImportDecl* ComponentDecl EOF`.
    ComponentEntry,

    // Declarations.
    /// `import ModulePath ImportSuffix? ";"`.
    ImportDecl,
    /// A `::`-separated module path `IDENT ("::" IDENT)*`.
    ModulePath,
    /// One `{ a, b as c }` selective-import item.
    ImportItem,
    /// The renamed-import tail `"as" IDENT` on an import or item.
    RenameClause,
    /// An `export`-prefixed top-level declaration; visibility travels with the
    /// wrapped declaration.
    ExportDecl,
    /// A leading `@Path(args)` attribute.
    Attribute,
    /// The parenthesized argument list of an attribute.
    AttributeArgs,
    /// `record IDENT ... "{" RecordField* "}"`.
    RecordDecl,
    /// `Attribute* IDENT ":" Type ("=" Expr)? ";"` inside a record.
    RecordField,
    /// `enum IDENT ... "{" EnumVariant* "}"`.
    EnumDecl,
    /// `Attribute* IDENT VariantPayload? ";"` inside an enum.
    EnumVariant,
    /// A tuple or record payload attached to an enum variant.
    VariantPayload,
    /// `const IDENT ":" Type "=" Expr ";"`.
    ConstDecl,
    /// `type IDENT GenericParams? "=" Type ";"`.
    TypeAliasDecl,
    /// `component IDENT ... "{" ComponentMember* "}"`.
    ComponentDecl,
    /// `system IDENT ... "{" ... "}"` — a scheduler system declaration.
    SystemDecl,
    /// `fn IDENT GenericParams? "(" ParamList ")" ReturnType? ... Block`.
    FnDecl,
    /// `action IDENT "(" ParamList ")" ReturnType? Block`.
    ActionDecl,
    /// `task IDENT "(" ParamList ")" ReturnType? Block`.
    TaskDecl,
    /// `input IDENT ":" Type ("=" Expr)? ";"`.
    InputDecl,
    /// `state IDENT (":" Type)? "=" Expr ";"` — `=` is initialization.
    StateDecl,
    /// `computed IDENT (":" Type)? "=" Expr ";"`.
    ComputedDecl,
    /// `event IDENT "(" EventParam* ")" ";"`.
    EventDecl,
    /// One `IDENT ":" Type` inside an event parameter list.
    EventParam,
    /// `slot IDENT ":" Type ("=" SlotDefault)? ";"`.
    SlotDecl,
    /// `effect IDENT EffectDeps? EffectRun? EffectBody`.
    EffectDecl,
    /// `when "(" Expr ("," Expr)* ","? ")"` — an effect's dependency list.
    EffectDeps,
    /// `run Path` — an effect's run policy.
    EffectRun,
    /// `"{" Statement* CleanupClause? "}"` — an effect's body.
    EffectBody,
    /// `cleanup Block` — what an effect undoes before it re-runs or ends.
    CleanupClause,
    /// `resource IDENT ":" Type "{" ResourceItem* "}"`.
    ResourceDecl,
    /// `load = Expr ;` — the task call a resource loads by.
    ResourceLoad,
    /// `key = Expr ;` — the value a resource is loaded for.
    ResourceKey,
    /// `policy = Expr ;` — a resource's policies.
    ResourcePolicy,
    /// `scope = Expr ;` — where a resource's cache lives.
    ResourceScope,
    /// `theme IDENT ThemeBase? "{" ThemeItem* "}"`.
    ThemeDecl,
    /// `":" TypePath` — the theme a theme starts from.
    ThemeBase,
    /// `IDENT "=" Expr ";"` — one field of a theme.
    ThemeItem,
    /// `style IDENT for TypePath StyleBases? "{" (PropertyBinding | StyleWhen)* "}"`.
    StyleDecl,
    /// `":" TypePath ("+" TypePath)*` — the styles a style applies first.
    StyleBases,
    /// `when Expr "{" PropertyBinding* "}"` — what a style sets while a
    /// selector holds.
    StyleWhen,

    // Generics / parameters / types.
    /// A `< ... >` list of generic parameters on a declaration.
    GenericParams,
    /// One generic parameter (a type or const parameter).
    GenericParam,
    /// An `implements Trait (+ Trait)*` clause.
    ImplementsClause,
    /// A `where` clause.
    WhereClause,
    /// A `requires { ... }` capability clause.
    CapabilityClause,
    /// A `"(" Parameter,* ")"` parameter list.
    ParamList,
    /// One `"mut"? IDENT ":" Type ("=" Expr)?` parameter.
    Param,
    /// A `"->" Type` return type.
    ReturnType,
    /// A `TypePathSegment ("::" TypePathSegment)*` type path.
    TypePath,
    /// One `IDENT GenericArgs?` segment of a type path.
    TypePathSegment,
    /// A `< GenericArg,* >` list of generic arguments.
    GenericArgs,
    /// A tuple type `"(" Type,* ")"`.
    TupleType,
    /// An array or slice type `"[" Type (";" Expr)? "]"`.
    ArrayType,
    /// A trait-object type `"dyn" TypePath`.
    DynType,

    // View.
    /// `view ViewBlock`.
    ViewDecl,
    /// A `"{" ViewStructureItem* "}"` view block.
    ViewBlock,
    /// `node IDENT ":" ComponentType NodeBody`.
    NamedNode,
    /// `ComponentType NodeBody` — an anonymous child node.
    AnonymousNode,
    /// `part IDENT ":" ComponentType NodeBody`.
    PartNode,
    /// `use TypePath "(" ArgumentList ")" NodeBody? ";"` — a template use.
    TemplateUse,
    /// `override part IDENT "{" PartOverrideItem* "}"`.
    PartOverride,
    /// `replace part IDENT ViewBlock`.
    PartReplace,
    /// A `"{" NodeMember* "}"` node body.
    NodeBody,
    /// `PropertyPath ":" Expr ";"` — a property binding.
    PropertyBinding,
    /// An `IDENT ("." IDENT)*` property path.
    PropertyPath,
    /// `bind PropertyPath "<=>" AssignablePath ("using" TypePath)? ";"`.
    TwoWayBinding,
    /// `on EventPhase? IDENT ("(" Pattern ")")? Block`.
    EventHandler,
    /// `fill IDENT ViewBlock`.
    FillClause,
    /// `if HeadExpr ("preserve" STRING)? ViewBlock ("else" ...)?`.
    ViewIf,
    /// `for Pattern "in" HeadExpr "key" HeadExpr ViewBlock`.
    ViewFor,
    /// `match HeadExpr "{" ViewMatchArm,* "}"`.
    ViewMatch,
    /// `Pattern ("if" Expr)? "=>" ViewBlock` inside a view match.
    ViewMatchArm,

    // Statements.
    /// `let "mut"? Pattern (":" Type)? "=" Expr ";"`.
    LetStmt,
    /// `AssignablePath AssignmentOperator Expr ";"`.
    AssignStmt,
    /// An `AssignablePath` (the left side of an assignment).
    AssignablePath,
    /// `Expr ";"` — an expression statement.
    ExprStmt,
    /// `return Expr? ";"`.
    ReturnStmt,
    /// `break Expr? ";"`.
    BreakStmt,
    /// `continue ";"`.
    ContinueStmt,
    /// `emit IDENT "(" ArgumentList ")" ";"`.
    EmitStmt,
    /// `transaction Block`.
    TransactionStmt,
    /// `start CallExpr StartSlot? StartHandlers? ";"`.
    StartStmt,
    /// `as IDENT` — the task slot a `start` runs in.
    StartSlot,
    /// `"{" (StartPolicy | StartSuccess | StartError | StartCancelled)* "}"`.
    StartHandlers,
    /// `policy = Expr ;`.
    StartPolicy,
    /// `success ( Pattern ) Block`.
    StartSuccess,
    /// `error ( Pattern ) Block`.
    StartError,
    /// `cancelled Block`.
    StartCancelled,
    /// `while HeadExpr Block`.
    WhileStmt,
    /// `for Pattern "in" HeadExpr Block` — a behavior loop (no key).
    ForStmt,
    /// `loop Block`.
    LoopStmt,
    /// `if HeadExpr Block ("else" (IfStmt | Block))?` — statement form.
    IfStmt,
    /// A `match` used in statement position.
    MatchStmt,

    // Expressions.
    /// A literal expression (int/float/string/color/bool/None/…).
    LiteralExpr,
    /// A string with holes: `StringStart`, then each hole's expression before a
    /// `StringMiddle` or the closing `StringEnd`.
    InterpolatedString,
    /// A path expression `Path` / `self` / `Self`.
    PathExpr,
    /// A binary operator expression `lhs op rhs`.
    BinaryExpr,
    /// A prefix unary expression `op expr`.
    UnaryExpr,
    /// A `expr "as" Type` cast.
    CastExpr,
    /// A range expression `lo (".." | "..=") hi`.
    RangeExpr,
    /// A call `callee GenericCallArgs? "(" ArgumentList ")"`.
    CallExpr,
    /// An index `expr "[" Expr "]"`.
    IndexExpr,
    /// A field access `expr "." IDENT`.
    FieldExpr,
    /// An optional field access `expr "?." IDENT`.
    OptionalFieldExpr,
    /// A try `expr "?"`.
    TryExpr,
    /// A turbofish `"::" GenericArgs` on a call.
    GenericCallArgs,
    /// A `"(" Expr,* ")"` — parenthesized expr or tuple.
    ParenExpr,
    /// A tuple expression `"(" Expr "," ... ")"`.
    TupleExpr,
    /// A list expression `"[" Expr,* "]"`.
    ListExpr,
    /// A record expression `Path GenericCallArgs? "{" RecordInitializer,* "}"`.
    RecordExpr,
    /// One `IDENT (":" Expr)?` or `".." Expr` record initializer.
    RecordExprField,
    /// An `if ... else ...` expression (both arms required).
    IfExpr,
    /// A `match` expression.
    MatchExpr,
    /// `Pattern ("if" Expr)? "=>" (Expr | Block)` in a match expression.
    MatchArm,
    /// A closure `"move"? (ClosureParams | "||") ... (Expr | Block)`.
    ClosureExpr,
    /// A `"|" ClosureParameter,* "|"` closure parameter list.
    ClosureParams,
    /// One `"mut"? Pattern (":" Type)?` closure parameter.
    ClosureParam,
    /// An `IDENT ":" Expr` or positional `Expr` call argument.
    Argument,
    /// The argument list inside a call `"(" Argument,* ")"`.
    ArgumentList,

    // Patterns.
    /// A top-level pattern; wraps the specific pattern shapes below.
    Pattern,
    /// An `a | b | c` or-pattern.
    OrPattern,
    /// An `IDENT "@" Pattern` binding pattern.
    BindingPattern,
    /// A `lo (".." | "..=") hi` range pattern.
    RangePattern,
    /// The wildcard `_` pattern.
    WildcardPattern,
    /// A literal pattern.
    LiteralPattern,
    /// An `"mut"? IDENT` identifier (binding) pattern.
    IdentPattern,
    /// A `"(" Pattern,* ")"` tuple pattern.
    TuplePattern,
    /// A `"[" ListPatternItem,* "]"` list pattern.
    ListPattern,
    /// A `TypePath (…)`/`{…}` constructor pattern.
    ConstructorPattern,
    /// A `IDENT "::" IDENT (…)` qualified-variant pattern.
    QualifiedVariantPattern,
    /// One `IDENT (":" Pattern)?` / `".."` field in a record constructor pattern.
    RecordPatternField,
    /// A `".." IDENT?` rest item inside a list pattern.
    RestPattern,
    /// A `"(" Pattern ")"` grouped pattern.
    ParenPattern,
    /// A `Block` in expression position (a `PrimaryExpression`).
    BlockExpr,
    /// A `"const" ConstExpression` generic argument.
    ConstGenericArg,

    // Shader Profile.
    /// `shader IDENT GenericParams? { ShaderMember* }`.
    ShaderDecl,
    /// `uniform IDENT : Type ;` in a shader.
    ShaderUniform,
    /// `instance IDENT : Type ;` in a shader.
    ShaderInstance,
    /// `varying IDENT : Type ;` in a shader.
    ShaderVarying,
    /// `texture IDENT : Type ;` in a shader.
    ShaderTexture,
    /// `sampler IDENT : Type ;` in a shader.
    ShaderSampler,
    /// `fn IDENT ( ParamList ) ReturnType Block` in a shader.
    ShaderFn,
    /// `vertex|fragment|compute ( ParamList ) ReturnType Block` in a shader.
    ShaderEntry,

    /// `native (fn|action|task) IDENT GenericParams? ( ParamList ) ReturnType?
    /// WhereClause? CapabilityClause? ;` or `native type IDENT GenericParams?
    /// (: TraitBounds)? WhereClause? ;`.
    NativeDecl,
    /// `trait IDENT GenericParams? (: TraitBounds)? WhereClause? { TraitMember* }`.
    TraitDecl,
    /// `impl GenericParams? Type (for Type)? WhereClause? { ImplMember* }`.
    ImplDecl,
    /// `type IDENT (: TraitBounds)? ;` in a trait, `type IDENT = Type ;` in an
    /// impl.
    AssocTypeDecl,

    /// `template IDENT GenericParams? ParamList WhereClause? "{" TemplateMember+
    /// "}"`.
    TemplateDecl,

    /// The component-member fragment entry: `ComponentMember* EOF` — members
    /// written without their component, as documentation and tooling quote them.
    MemberFragment,
    /// The node-member fragment entry: `NodeMember* EOF` — the inside of a node
    /// body without its braces.
    NodeMemberFragment,
    /// The block-body fragment entry: `Statement* TailExpression? EOF` — a block
    /// without its braces.
    BlockFragment,
}

impl SyntaxKind {
    /// The kind's name, as dumps and tools print it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Whitespace => "Whitespace",
            Self::LineComment => "LineComment",
            Self::BlockComment => "BlockComment",
            Self::DocComment => "DocComment",
            Self::ModuleDocComment => "ModuleDocComment",
            Self::IntLiteral => "IntLiteral",
            Self::FloatLiteral => "FloatLiteral",
            Self::UnitLiteral => "UnitLiteral",
            Self::StringLiteral => "StringLiteral",
            Self::StringStart => "StringStart",
            Self::StringMiddle => "StringMiddle",
            Self::StringEnd => "StringEnd",
            Self::RawStringLiteral => "RawStringLiteral",
            Self::CharLiteral => "CharLiteral",
            Self::ColorLiteral => "ColorLiteral",
            Self::Ident => "Ident",
            Self::RawIdent => "RawIdent",
            Self::ModuleQualifier => "ModuleQualifier",
            Self::ImportKw => "ImportKw",
            Self::ExportKw => "ExportKw",
            Self::AsKw => "AsKw",
            Self::ComponentKw => "ComponentKw",
            Self::SystemKw => "SystemKw",
            Self::RecordKw => "RecordKw",
            Self::EnumKw => "EnumKw",
            Self::TraitKw => "TraitKw",
            Self::ImplKw => "ImplKw",
            Self::TypeKw => "TypeKw",
            Self::ImplementsKw => "ImplementsKw",
            Self::WhereKw => "WhereKw",
            Self::ForKw => "ForKw",
            Self::InputKw => "InputKw",
            Self::StateKw => "StateKw",
            Self::ComputedKw => "ComputedKw",
            Self::EventKw => "EventKw",
            Self::SlotKw => "SlotKw",
            Self::ConstKw => "ConstKw",
            Self::FnKw => "FnKw",
            Self::ActionKw => "ActionKw",
            Self::EffectKw => "EffectKw",
            Self::TaskKw => "TaskKw",
            Self::ResourceKw => "ResourceKw",
            Self::ViewKw => "ViewKw",
            Self::StyleKw => "StyleKw",
            Self::ThemeKw => "ThemeKw",
            Self::TemplateKw => "TemplateKw",
            Self::PartKw => "PartKw",
            Self::NativeKw => "NativeKw",
            Self::RequiresKw => "RequiresKw",
            Self::CapabilityKw => "CapabilityKw",
            Self::LetKw => "LetKw",
            Self::MutKw => "MutKw",
            Self::ReturnKw => "ReturnKw",
            Self::BreakKw => "BreakKw",
            Self::ContinueKw => "ContinueKw",
            Self::IfKw => "IfKw",
            Self::ElseKw => "ElseKw",
            Self::MatchKw => "MatchKw",
            Self::WhileKw => "WhileKw",
            Self::LoopKw => "LoopKw",
            Self::InKw => "InKw",
            Self::OnKw => "OnKw",
            Self::CaptureKw => "CaptureKw",
            Self::BubbleKw => "BubbleKw",
            Self::EmitKw => "EmitKw",
            Self::TransactionKw => "TransactionKw",
            Self::StartKw => "StartKw",
            Self::AwaitKw => "AwaitKw",
            Self::MoveKw => "MoveKw",
            Self::WhenKw => "WhenKw",
            Self::RunKw => "RunKw",
            Self::CleanupKw => "CleanupKw",
            Self::SuccessKw => "SuccessKw",
            Self::ErrorKw => "ErrorKw",
            Self::CancelledKw => "CancelledKw",
            Self::NodeKw => "NodeKw",
            Self::FillKw => "FillKw",
            Self::BindKw => "BindKw",
            Self::UsingKw => "UsingKw",
            Self::UseKw => "UseKw",
            Self::OverrideKw => "OverrideKw",
            Self::ReplaceKw => "ReplaceKw",
            Self::PreserveKw => "PreserveKw",
            Self::KeyKw => "KeyKw",
            Self::LoadKw => "LoadKw",
            Self::PolicyKw => "PolicyKw",
            Self::ScopeKw => "ScopeKw",
            Self::EmptyKw => "EmptyKw",
            Self::ShaderKw => "ShaderKw",
            Self::VertexKw => "VertexKw",
            Self::FragmentKw => "FragmentKw",
            Self::ComputeKw => "ComputeKw",
            Self::UniformKw => "UniformKw",
            Self::InstanceKw => "InstanceKw",
            Self::VaryingKw => "VaryingKw",
            Self::TextureKw => "TextureKw",
            Self::SamplerKw => "SamplerKw",
            Self::TrueKw => "TrueKw",
            Self::FalseKw => "FalseKw",
            Self::NoneKw => "NoneKw",
            Self::SelfValueKw => "SelfValueKw",
            Self::SelfTypeKw => "SelfTypeKw",
            Self::DynKw => "DynKw",
            Self::ChildKw => "ChildKw",
            Self::StoreKw => "StoreKw",
            Self::MergeKw => "MergeKw",
            Self::ExtendKw => "ExtendKw",
            Self::InheritKw => "InheritKw",
            Self::ClassKw => "ClassKw",
            Self::MacroKw => "MacroKw",
            Self::UnsafeKw => "UnsafeKw",
            Self::ExternKw => "ExternKw",
            Self::StaticKw => "StaticKw",
            Self::YieldKw => "YieldKw",
            Self::TryKw => "TryKw",
            Self::LParen => "LParen",
            Self::RParen => "RParen",
            Self::LBrace => "LBrace",
            Self::RBrace => "RBrace",
            Self::LBracket => "LBracket",
            Self::RBracket => "RBracket",
            Self::Comma => "Comma",
            Self::Semi => "Semi",
            Self::Colon => "Colon",
            Self::ColonColon => "ColonColon",
            Self::At => "At",
            Self::Eq => "Eq",
            Self::PlusEq => "PlusEq",
            Self::MinusEq => "MinusEq",
            Self::StarEq => "StarEq",
            Self::SlashEq => "SlashEq",
            Self::PercentEq => "PercentEq",
            Self::AmpEq => "AmpEq",
            Self::PipeEq => "PipeEq",
            Self::CaretEq => "CaretEq",
            Self::ShlEq => "ShlEq",
            Self::ShrEq => "ShrEq",
            Self::Plus => "Plus",
            Self::Minus => "Minus",
            Self::Star => "Star",
            Self::Slash => "Slash",
            Self::Percent => "Percent",
            Self::Bang => "Bang",
            Self::Tilde => "Tilde",
            Self::Amp => "Amp",
            Self::Pipe => "Pipe",
            Self::Caret => "Caret",
            Self::Shl => "Shl",
            Self::Shr => "Shr",
            Self::EqEq => "EqEq",
            Self::Neq => "Neq",
            Self::Lt => "Lt",
            Self::Le => "Le",
            Self::Gt => "Gt",
            Self::Ge => "Ge",
            Self::AmpAmp => "AmpAmp",
            Self::PipePipe => "PipePipe",
            Self::QuestionQuestion => "QuestionQuestion",
            Self::Dot => "Dot",
            Self::QuestionDot => "QuestionDot",
            Self::Question => "Question",
            Self::DotDot => "DotDot",
            Self::DotDotEq => "DotDotEq",
            Self::Arrow => "Arrow",
            Self::FatArrow => "FatArrow",
            Self::BidiArrow => "BidiArrow",
            Self::Eof => "Eof",
            Self::Error => "Error",
            Self::Root => "Root",
            Self::ErrorNode => "ErrorNode",
            Self::MissingToken => "MissingToken",
            Self::Item => "Item",
            Self::Block => "Block",
            Self::CompilationUnit => "CompilationUnit",
            Self::ViewFragment => "ViewFragment",
            Self::ComponentEntry => "ComponentEntry",
            Self::ImportDecl => "ImportDecl",
            Self::ModulePath => "ModulePath",
            Self::ImportItem => "ImportItem",
            Self::RenameClause => "RenameClause",
            Self::ExportDecl => "ExportDecl",
            Self::Attribute => "Attribute",
            Self::AttributeArgs => "AttributeArgs",
            Self::RecordDecl => "RecordDecl",
            Self::RecordField => "RecordField",
            Self::EnumDecl => "EnumDecl",
            Self::EnumVariant => "EnumVariant",
            Self::VariantPayload => "VariantPayload",
            Self::ConstDecl => "ConstDecl",
            Self::TypeAliasDecl => "TypeAliasDecl",
            Self::ComponentDecl => "ComponentDecl",
            Self::SystemDecl => "SystemDecl",
            Self::FnDecl => "FnDecl",
            Self::ActionDecl => "ActionDecl",
            Self::TaskDecl => "TaskDecl",
            Self::InputDecl => "InputDecl",
            Self::StateDecl => "StateDecl",
            Self::ComputedDecl => "ComputedDecl",
            Self::EventDecl => "EventDecl",
            Self::EventParam => "EventParam",
            Self::SlotDecl => "SlotDecl",
            Self::EffectDecl => "EffectDecl",
            Self::EffectDeps => "EffectDeps",
            Self::EffectRun => "EffectRun",
            Self::EffectBody => "EffectBody",
            Self::CleanupClause => "CleanupClause",
            Self::ResourceDecl => "ResourceDecl",
            Self::ResourceLoad => "ResourceLoad",
            Self::ResourceKey => "ResourceKey",
            Self::ResourcePolicy => "ResourcePolicy",
            Self::ResourceScope => "ResourceScope",
            Self::ThemeDecl => "ThemeDecl",
            Self::ThemeBase => "ThemeBase",
            Self::ThemeItem => "ThemeItem",
            Self::StyleDecl => "StyleDecl",
            Self::StyleBases => "StyleBases",
            Self::StyleWhen => "StyleWhen",
            Self::GenericParams => "GenericParams",
            Self::GenericParam => "GenericParam",
            Self::ImplementsClause => "ImplementsClause",
            Self::WhereClause => "WhereClause",
            Self::CapabilityClause => "CapabilityClause",
            Self::ParamList => "ParamList",
            Self::Param => "Param",
            Self::ReturnType => "ReturnType",
            Self::TypePath => "TypePath",
            Self::TypePathSegment => "TypePathSegment",
            Self::GenericArgs => "GenericArgs",
            Self::TupleType => "TupleType",
            Self::ArrayType => "ArrayType",
            Self::DynType => "DynType",
            Self::ViewDecl => "ViewDecl",
            Self::ViewBlock => "ViewBlock",
            Self::NamedNode => "NamedNode",
            Self::AnonymousNode => "AnonymousNode",
            Self::PartNode => "PartNode",
            Self::TemplateUse => "TemplateUse",
            Self::PartOverride => "PartOverride",
            Self::PartReplace => "PartReplace",
            Self::NodeBody => "NodeBody",
            Self::PropertyBinding => "PropertyBinding",
            Self::PropertyPath => "PropertyPath",
            Self::TwoWayBinding => "TwoWayBinding",
            Self::EventHandler => "EventHandler",
            Self::FillClause => "FillClause",
            Self::ViewIf => "ViewIf",
            Self::ViewFor => "ViewFor",
            Self::ViewMatch => "ViewMatch",
            Self::ViewMatchArm => "ViewMatchArm",
            Self::LetStmt => "LetStmt",
            Self::AssignStmt => "AssignStmt",
            Self::AssignablePath => "AssignablePath",
            Self::ExprStmt => "ExprStmt",
            Self::ReturnStmt => "ReturnStmt",
            Self::BreakStmt => "BreakStmt",
            Self::ContinueStmt => "ContinueStmt",
            Self::EmitStmt => "EmitStmt",
            Self::TransactionStmt => "TransactionStmt",
            Self::StartStmt => "StartStmt",
            Self::StartSlot => "StartSlot",
            Self::StartHandlers => "StartHandlers",
            Self::StartPolicy => "StartPolicy",
            Self::StartSuccess => "StartSuccess",
            Self::StartError => "StartError",
            Self::StartCancelled => "StartCancelled",
            Self::WhileStmt => "WhileStmt",
            Self::ForStmt => "ForStmt",
            Self::LoopStmt => "LoopStmt",
            Self::IfStmt => "IfStmt",
            Self::MatchStmt => "MatchStmt",
            Self::LiteralExpr => "LiteralExpr",
            Self::InterpolatedString => "InterpolatedString",
            Self::PathExpr => "PathExpr",
            Self::BinaryExpr => "BinaryExpr",
            Self::UnaryExpr => "UnaryExpr",
            Self::CastExpr => "CastExpr",
            Self::RangeExpr => "RangeExpr",
            Self::CallExpr => "CallExpr",
            Self::IndexExpr => "IndexExpr",
            Self::FieldExpr => "FieldExpr",
            Self::OptionalFieldExpr => "OptionalFieldExpr",
            Self::TryExpr => "TryExpr",
            Self::GenericCallArgs => "GenericCallArgs",
            Self::ParenExpr => "ParenExpr",
            Self::TupleExpr => "TupleExpr",
            Self::ListExpr => "ListExpr",
            Self::RecordExpr => "RecordExpr",
            Self::RecordExprField => "RecordExprField",
            Self::IfExpr => "IfExpr",
            Self::MatchExpr => "MatchExpr",
            Self::MatchArm => "MatchArm",
            Self::ClosureExpr => "ClosureExpr",
            Self::ClosureParams => "ClosureParams",
            Self::ClosureParam => "ClosureParam",
            Self::Argument => "Argument",
            Self::ArgumentList => "ArgumentList",
            Self::Pattern => "Pattern",
            Self::OrPattern => "OrPattern",
            Self::BindingPattern => "BindingPattern",
            Self::RangePattern => "RangePattern",
            Self::WildcardPattern => "WildcardPattern",
            Self::LiteralPattern => "LiteralPattern",
            Self::IdentPattern => "IdentPattern",
            Self::TuplePattern => "TuplePattern",
            Self::ListPattern => "ListPattern",
            Self::ConstructorPattern => "ConstructorPattern",
            Self::QualifiedVariantPattern => "QualifiedVariantPattern",
            Self::RecordPatternField => "RecordPatternField",
            Self::RestPattern => "RestPattern",
            Self::ParenPattern => "ParenPattern",
            Self::BlockExpr => "BlockExpr",
            Self::ConstGenericArg => "ConstGenericArg",
            Self::ShaderDecl => "ShaderDecl",
            Self::ShaderUniform => "ShaderUniform",
            Self::ShaderInstance => "ShaderInstance",
            Self::ShaderVarying => "ShaderVarying",
            Self::ShaderTexture => "ShaderTexture",
            Self::ShaderSampler => "ShaderSampler",
            Self::ShaderFn => "ShaderFn",
            Self::ShaderEntry => "ShaderEntry",
            Self::NativeDecl => "NativeDecl",
            Self::TraitDecl => "TraitDecl",
            Self::ImplDecl => "ImplDecl",
            Self::AssocTypeDecl => "AssocTypeDecl",
            Self::TemplateDecl => "TemplateDecl",
            Self::MemberFragment => "MemberFragment",
            Self::NodeMemberFragment => "NodeMemberFragment",
            Self::BlockFragment => "BlockFragment",
        }
    }

    /// Whether this kind is a type node.
    #[inline]
    pub fn is_type(self) -> bool {
        matches!(
            self,
            Self::TypePath | Self::TupleType | Self::ArrayType | Self::DynType
        )
    }

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

    /// Whether this kind is a keyword kind, strict or contextual. Context words
    /// are `Ident`, so they return `false`.
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
    /// (`CompilationUnit`..=`TemplateDecl`). Everything below `CompilationUnit`
    /// is a token kind. A single lower-bound comparison covers the whole tail
    /// because no token kind is appended after the nodes.
    #[inline]
    pub fn is_node(self) -> bool {
        self as u16 >= Self::Root as u16
    }

    /// Whether this kind is one of the declaration keywords the skeleton parser
    /// treats as an item boundary and a recovery sync point.
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

    /// Whether this kind is a contextual keyword. The lexer
    /// never produces one: the word lexes as [`Self::Ident`], and the parser
    /// retags it only where its keyword position recognizes it.
    #[inline]
    pub fn is_contextual_keyword(self) -> bool {
        matches!(
            self,
            Self::InputKw
                | Self::StateKw
                | Self::ComputedKw
                | Self::EventKw
                | Self::SlotKw
                | Self::EffectKw
                | Self::ResourceKw
                | Self::ViewKw
                | Self::StyleKw
                | Self::ThemeKw
                | Self::TemplateKw
                | Self::PartKw
                | Self::RequiresKw
                | Self::CapabilityKw
                | Self::OnKw
                | Self::CaptureKw
                | Self::BubbleKw
                | Self::EmitKw
                | Self::TransactionKw
                | Self::StartKw
                | Self::MoveKw
                | Self::WhenKw
                | Self::RunKw
                | Self::CleanupKw
                | Self::SuccessKw
                | Self::ErrorKw
                | Self::CancelledKw
                | Self::NodeKw
                | Self::FillKw
                | Self::BindKw
                | Self::UsingKw
                | Self::UseKw
                | Self::OverrideKw
                | Self::ReplaceKw
                | Self::PreserveKw
                | Self::KeyKw
                | Self::LoadKw
                | Self::PolicyKw
                | Self::ScopeKw
                | Self::EmptyKw
                | Self::VertexKw
                | Self::FragmentKw
                | Self::ComputeKw
                | Self::UniformKw
                | Self::InstanceKw
                | Self::VaryingKw
                | Self::TextureKw
                | Self::SamplerKw
        )
    }

    /// Whether this kind is a strict keyword: a word
    /// that can never be a binding or declaration name.
    #[inline]
    pub fn is_strict_keyword(self) -> bool {
        self.is_keyword() && !self.is_contextual_keyword()
    }

    /// The contextual keyword `text` spells, if any. Used by
    /// the parser at the contextual-keyword recognition positions.
    #[inline]
    pub fn contextual_keyword(text: &str) -> Option<SyntaxKind> {
        Some(match text {
            "input" => Self::InputKw,
            "state" => Self::StateKw,
            "computed" => Self::ComputedKw,
            "event" => Self::EventKw,
            "slot" => Self::SlotKw,
            "effect" => Self::EffectKw,
            "resource" => Self::ResourceKw,
            "view" => Self::ViewKw,
            "style" => Self::StyleKw,
            "theme" => Self::ThemeKw,
            "template" => Self::TemplateKw,
            "part" => Self::PartKw,
            "requires" => Self::RequiresKw,
            "capability" => Self::CapabilityKw,
            "on" => Self::OnKw,
            "capture" => Self::CaptureKw,
            "bubble" => Self::BubbleKw,
            "emit" => Self::EmitKw,
            "transaction" => Self::TransactionKw,
            "start" => Self::StartKw,
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
            "empty" => Self::EmptyKw,
            "vertex" => Self::VertexKw,
            "fragment" => Self::FragmentKw,
            "compute" => Self::ComputeKw,
            "uniform" => Self::UniformKw,
            "instance" => Self::InstanceKw,
            "varying" => Self::VaryingKw,
            "texture" => Self::TextureKw,
            "sampler" => Self::SamplerKw,
            _ => return None,
        })
    }

    /// Classify an identifier's text as its strict keyword kind, or
    /// [`Self::Ident`] if it is not one; contextual keywords lex as identifiers. A closed `match` on the byte string — no map, no
    /// allocation — because the keyword set is small and fixed.
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
            "const" => Self::ConstKw,
            "fn" => Self::FnKw,
            "action" => Self::ActionKw,
            "task" => Self::TaskKw,
            "native" => Self::NativeKw,

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
            "await" => Self::AwaitKw,
            "shader" => Self::ShaderKw,

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
