//! Typed AST wrappers over the green tree.
//!
//! Following rust-analyzer's idiom, the AST is a set of *typed views*, not a
//! separate owned tree. Each wrapper is a newtype over a [`SyntaxNode`] plus a
//! kind check; [`AstNode::cast`] returns `Some` only when the node's kind matches,
//! and typed accessors filter that node's children (see [`support`](super::support)).
//! The green tree stays the single source of truth — the same tree a formatter, an
//! LSP, or a rename refactor walks — so projecting the AST costs nothing beyond a
//! kind tag comparison.

use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::support;

/// A typed view over a [`SyntaxNode`] of one specific [`SyntaxKind`] (or a fixed
/// set, for the expression/declaration enums).
///
/// `cast` is the sole constructor: it validates the node's kind and returns `None`
/// otherwise, so a wrapper always wraps a node of the right shape. `syntax` returns
/// the underlying node for navigation, ranges, and losslessness.
pub trait AstNode: Sized {
    /// Whether a node of `kind` can be cast to this wrapper.
    fn can_cast(kind: SyntaxKind) -> bool;

    /// Wraps `node` if its kind matches, else `None`.
    fn cast(node: SyntaxNode) -> Option<Self>;

    /// The underlying syntax node.
    fn syntax(&self) -> &SyntaxNode;
}

/// Declares a wrapper newtype over a single [`SyntaxKind`] plus its [`AstNode`]
/// impl. Accessors are added in a separate `impl` block below.
macro_rules! ast_node {
    ($(#[$m:meta])* $name:ident = $kind:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name {
            syntax: SyntaxNode,
        }

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == SyntaxKind::$kind
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                if Self::can_cast(node.kind()) {
                    Some($name { syntax: node })
                } else {
                    None
                }
            }
            fn syntax(&self) -> &SyntaxNode {
                &self.syntax
            }
        }
    };
}

// --- Roots -------------------------------------------------------------------

ast_node!(
    /// A `.vs` file / `view!` entry: imports then top-level declarations.
    CompilationUnit = CompilationUnit
);
ast_node!(
    /// A `ui!` bare view fragment: view items with no component/view wrapper.
    ViewFragment = ViewFragment
);
ast_node!(
    /// A `component!` entry: imports then one component declaration.
    ComponentEntry = ComponentEntry
);

impl CompilationUnit {
    /// The `import` declarations at the head of the unit.
    pub fn imports(&self) -> impl Iterator<Item = ImportDecl> {
        support::children(&self.syntax)
    }

    /// The top-level declarations, in source order. `export`-prefixed
    /// declarations appear as [`Item::Export`]; the wrapped declaration is
    /// reached through [`ExportDecl::declaration`].
    pub fn items(&self) -> impl Iterator<Item = Item> {
        support::children(&self.syntax)
    }
}

impl ViewFragment {
    /// The view structure items directly under the fragment.
    pub fn items(&self) -> impl Iterator<Item = ViewItem> {
        support::children(&self.syntax)
    }
}

impl ComponentEntry {
    /// The single component this entry declares, if it parsed.
    pub fn component(&self) -> Option<ComponentDecl> {
        support::child(&self.syntax)
    }
}

// --- Declarations ------------------------------------------------------------

ast_node!(
    /// `import ModulePath (as IDENT | ::{items})? ;`
    ImportDecl = ImportDecl
);
ast_node!(
    /// A `::`-separated module path.
    ModulePath = ModulePath
);
ast_node!(
    /// One `a` / `a as b` selective-import item inside `::{ ... }`.
    ImportItem = ImportItem
);
ast_node!(
    /// The `as IDENT` rename tail on an import or item.
    RenameClause = RenameClause
);
ast_node!(
    /// An `export`-prefixed declaration; visibility travels with the wrapped decl.
    ExportDecl = ExportDecl
);
ast_node!(
    /// `component IDENT ... { member* }`.
    ComponentDecl = ComponentDecl
);
ast_node!(
    /// `system IDENT ... { member* }`.
    SystemDecl = SystemDecl
);
ast_node!(
    /// `record IDENT ... { field* }`.
    RecordDecl = RecordDecl
);
ast_node!(
    /// One `IDENT : Type (= Expr)? ;` record field.
    RecordField = RecordField
);
ast_node!(
    /// `enum IDENT ... { variant* }`.
    EnumDecl = EnumDecl
);
ast_node!(
    /// One `IDENT VariantPayload? ;` enum variant.
    EnumVariant = EnumVariant
);
ast_node!(
    /// `input IDENT : Type (= Expr)? ;`.
    InputDecl = InputDecl
);
ast_node!(
    /// `state IDENT (: Type)? = Expr ;`.
    StateDecl = StateDecl
);
ast_node!(
    /// `computed IDENT (: Type)? = Expr ;`.
    ComputedDecl = ComputedDecl
);
ast_node!(
    /// `event IDENT ( EventParam* ) ;`.
    EventDecl = EventDecl
);
ast_node!(
    /// `slot IDENT : Type (= None | = empty)? ;`.
    SlotDecl = SlotDecl
);
ast_node!(
    /// `effect IDENT (when (Expr,*))? (run Path)? { Statement* (cleanup Block)? }`.
    EffectDecl = EffectDecl
);
ast_node!(
    /// `when ( Expr, ... )` — an effect's dependencies.
    EffectDeps = EffectDeps
);
ast_node!(
    /// `resource IDENT : Type { ResourceItem* }`.
    ResourceDecl = ResourceDecl
);
ast_node!(
    /// `theme IDENT (: TypePath)? { ThemeItem* }`.
    ThemeDecl = ThemeDecl
);
ast_node!(
    /// `IDENT = Expr ;` in a theme.
    ThemeItem = ThemeItem
);
ast_node!(
    /// `style IDENT for TypePath (: TypePath (+ TypePath)*)? { .. }`.
    StyleDecl = StyleDecl
);
ast_node!(
    /// `when Expr { PropertyBinding* }` in a style.
    StyleWhen = StyleWhen
);
/// `load = Expr ;`, `key = Expr ;`, `policy = Expr ;` or `scope = Expr ;`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceItem {
    syntax: SyntaxNode,
}

impl AstNode for ResourceItem {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::ResourceLoad
                | SyntaxKind::ResourceKey
                | SyntaxKind::ResourcePolicy
                | SyntaxKind::ResourceScope
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        Self::can_cast(node.kind()).then_some(ResourceItem { syntax: node })
    }
    fn syntax(&self) -> &SyntaxNode {
        &self.syntax
    }
}
ast_node!(
    /// `run Path` — an effect's run policy.
    EffectRun = EffectRun
);
ast_node!(
    /// `{ Statement* (cleanup Block)? }` — an effect's body.
    EffectBody = EffectBody
);
ast_node!(
    /// `cleanup Block`.
    CleanupClause = CleanupClause
);
ast_node!(
    /// `start CallExpr (as IDENT)? StartHandlers? ;`.
    StartStmt = StartStmt
);
ast_node!(
    /// `as IDENT` — a `start`'s task slot.
    StartSlot = StartSlot
);
ast_node!(
    /// `{ policy = ..; success(p) {} error(p) {} cancelled {} }`.
    StartHandlers = StartHandlers
);
ast_node!(
    /// `policy = Expr ;`.
    StartPolicy = StartPolicy
);
ast_node!(
    /// `success ( Pattern ) Block`.
    StartSuccess = StartSuccess
);
ast_node!(
    /// `error ( Pattern ) Block`.
    StartError = StartError
);
ast_node!(
    /// `cancelled Block`.
    StartCancelled = StartCancelled
);
ast_node!(
    /// `const IDENT : Type = Expr ;`.
    ConstDecl = ConstDecl
);
ast_node!(
    /// `type IDENT GenericParams? = Type ;`.
    TypeAliasDecl = TypeAliasDecl
);
ast_node!(
    /// `fn IDENT ( ParamList ) ReturnType? ... Block`.
    FnDecl = FnDecl
);
ast_node!(
    /// `action IDENT ( ParamList ) ReturnType? Block`.
    ActionDecl = ActionDecl
);
ast_node!(
    /// `task IDENT ( ParamList ) ReturnType? Block`.
    TaskDecl = TaskDecl
);
ast_node!(
    /// `view ViewBlock` inside a component/system.
    ViewDecl = ViewDecl
);
ast_node!(
    /// `shader IDENT GenericParams? { ShaderMember* }` (§97).
    ShaderDecl = ShaderDecl
);
ast_node!(
    /// `uniform IDENT : Type ;` in a shader.
    ShaderUniform = ShaderUniform
);
ast_node!(
    /// `instance IDENT : Type ;` in a shader.
    ShaderInstance = ShaderInstance
);
ast_node!(
    /// `varying IDENT : Type ;` in a shader.
    ShaderVarying = ShaderVarying
);
ast_node!(
    /// `texture IDENT : Type ;` in a shader.
    ShaderTexture = ShaderTexture
);
ast_node!(
    /// `sampler IDENT : Type ;` in a shader.
    ShaderSampler = ShaderSampler
);
ast_node!(
    /// `fn IDENT ( ParamList ) ReturnType Block` in a shader.
    ShaderFn = ShaderFn
);
ast_node!(
    /// `vertex|fragment|compute ( ParamList ) ReturnType Block` in a shader.
    ShaderEntry = ShaderEntry
);
ast_node!(
    /// `native (fn|action|task|type) IDENT … ;` — a handwritten native
    /// declaration (§47).
    NativeDecl = NativeDecl
);
ast_node!(
    /// `trait IDENT GenericParams? (: TraitBounds)? WhereClause? { TraitMember* }`.
    TraitDecl = TraitDecl
);
ast_node!(
    /// `impl GenericParams? Type (for Type)? WhereClause? { ImplMember* }`.
    ImplDecl = ImplDecl
);
ast_node!(
    /// `type IDENT (: TraitBounds)? ;` in a trait, `type IDENT = Type ;` in an impl.
    AssocTypeDecl = AssocTypeDecl
);
ast_node!(
    /// `template IDENT GenericParams? ( ParamList ) WhereClause? { Member* }` (§58).
    TemplateDecl = TemplateDecl
);
ast_node!(
    /// `part IDENT : ComponentType NodeBody` — a node a template or component
    /// exposes to its callers (§57).
    PartNode = PartNode
);
ast_node!(
    /// `use TypePath ( ArgumentList ) NodeBody? ;` — a template use (§58).
    TemplateUse = TemplateUse
);
ast_node!(
    /// `override part IDENT { PartOverrideItem* }` (§57).
    PartOverride = PartOverride
);
ast_node!(
    /// `replace part IDENT ViewBlock` (§57).
    PartReplace = PartReplace
);

impl TemplateDecl {
    /// The template's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// Its parameters, in order.
    pub fn params(&self) -> Vec<Param> {
        support::child::<ParamList>(&self.syntax)
            .map(|l| l.params().collect())
            .unwrap_or_default()
    }

    /// The template as a component: the same node, whose parameters are its
    /// inputs and whose members lower like a component's. Lowering rejects
    /// any member but `slot`, `const`, `fn` and `view`.
    pub fn as_component(&self) -> ComponentDecl {
        ComponentDecl {
            syntax: self.syntax.clone(),
        }
    }
}

impl PartNode {
    /// The part's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The part's node type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The part's body.
    pub fn body(&self) -> Option<NodeBody> {
        support::child(&self.syntax)
    }
}

impl TemplateUse {
    /// The template the use names.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// Its arguments in order: each value, with the label a named one has.
    pub fn args(&self) -> Vec<(Option<SyntaxToken>, Expr)> {
        self.syntax
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::ArgumentList)
            .flat_map(|list| list.children())
            .filter(|a| a.kind() == SyntaxKind::Argument)
            .filter_map(|arg| {
                let value = arg.children().into_iter().find_map(Expr::cast)?;
                let label = arg
                    .children_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent));
                Some((label, value))
            })
            .collect()
    }

    /// Its argument list, if written.
    pub fn arg_list(&self) -> Option<SyntaxNode> {
        self.syntax
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::ArgumentList)
    }

    /// Its body: the `fill`, `override part` and `replace part` clauses.
    pub fn body(&self) -> Option<NodeBody> {
        support::child(&self.syntax)
    }
}

impl PartOverride {
    /// The name of the part it overrides.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// Its bindings and handlers, in order.
    pub fn members(&self) -> impl Iterator<Item = ViewItem> {
        support::children(&self.syntax)
    }
}

impl PartReplace {
    /// The name of the part it replaces.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// What replaces the part.
    pub fn body(&self) -> Option<ViewBlock> {
        support::child(&self.syntax)
    }
}

impl ImportDecl {
    /// The imported module path.
    pub fn path(&self) -> Option<ModulePath> {
        support::child(&self.syntax)
    }

    /// The `as IDENT` rename, when the whole module is renamed.
    pub fn rename(&self) -> Option<RenameClause> {
        support::child(&self.syntax)
    }

    /// The selective `::{ a, b as c }` items, when present.
    pub fn items(&self) -> impl Iterator<Item = ImportItem> {
        support::children(&self.syntax)
    }
}

impl ModulePath {
    /// The path segments, each an identifier token, in order.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
    }
}

impl ImportItem {
    /// The item's own name token.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The `as IDENT` rename on this item, if any.
    pub fn rename(&self) -> Option<RenameClause> {
        support::child(&self.syntax)
    }
}

impl RenameClause {
    /// The new name introduced by `as`.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

impl ExportDecl {
    /// The declaration this `export` makes public.
    pub fn declaration(&self) -> Option<Item> {
        support::child(&self.syntax)
    }
}

impl ComponentDecl {
    /// The component's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The component's members (inputs, state, actions, view, ...), in order.
    pub fn members(&self) -> impl Iterator<Item = Member> {
        support::children(&self.syntax)
    }

    /// The component's `view` declaration, if it has one.
    pub fn view(&self) -> Option<ViewDecl> {
        support::child(&self.syntax)
    }

    /// Whether this is a template seen as a component
    /// ([`TemplateDecl::as_component`]).
    pub fn is_template(&self) -> bool {
        self.syntax.kind() == SyntaxKind::TemplateDecl
    }

    /// A template's parameters, its inputs; none for a component.
    pub fn template_params(&self) -> Vec<Param> {
        TemplateDecl::cast(self.syntax.clone())
            .map(|t| t.params())
            .unwrap_or_default()
    }
}

impl SystemDecl {
    /// The system's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The system's members, in order.
    pub fn members(&self) -> impl Iterator<Item = Member> {
        support::children(&self.syntax)
    }

    /// The trait bounds of its `implements` clause, in order.
    pub fn implements(&self) -> impl Iterator<Item = TypePath> {
        self.syntax
            .children()
            .into_iter()
            .filter(|n| n.kind() == SyntaxKind::ImplementsClause)
            .flat_map(|clause| clause.children())
            .filter_map(TypePath::cast)
    }

    /// Its `@after(..)` and `@before(..)` attributes, in source order: which
    /// way each orders the system, the attribute and the name each argument
    /// gives, `None` for an argument that is not one bare name.
    pub fn ordering(&self) -> Vec<(SystemOrder, SyntaxNode, Vec<Option<SyntaxToken>>)> {
        decl_attributes(&self.syntax)
            .into_iter()
            .filter_map(|(name, attr, args)| {
                let order = match name.as_str() {
                    "after" => SystemOrder::After,
                    "before" => SystemOrder::Before,
                    _ => return None,
                };
                Some((order, attr, args))
            })
            .collect()
    }

    /// The system as a component: the same node, whose members lower like a
    /// component's. Lowering rejects a system's `view`, `event` and `slot`
    /// members.
    pub fn as_component(&self) -> ComponentDecl {
        ComponentDecl {
            syntax: self.syntax.clone(),
        }
    }
}

/// The attributes of the declaration `decl`: the run of attributes preceding
/// it or its `export`, in source order, each with its name, its node and the
/// name each argument gives, `None` for an argument that is not one bare name.
pub fn decl_attributes(decl: &SyntaxNode) -> Vec<(String, SyntaxNode, Vec<Option<SyntaxToken>>)> {
    let decl = decl
        .parent()
        .filter(|p| p.kind() == SyntaxKind::ExportDecl)
        .unwrap_or_else(|| decl.clone());
    let mut attrs: Vec<SyntaxNode> =
        std::iter::successors(decl.prev_sibling(), SyntaxNode::prev_sibling)
            .take_while(|n| n.kind() == SyntaxKind::Attribute)
            .collect();
    attrs.reverse();
    attrs
        .into_iter()
        .filter_map(|attr| {
            let path = attr
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::PathExpr)?;
            let name = path.text().to_string().trim().to_owned();
            let args = attr
                .children()
                .into_iter()
                .filter(|c| c.kind() == SyntaxKind::ArgumentList)
                .flat_map(|list| list.children())
                .filter(|c| c.kind() == SyntaxKind::Argument)
                .map(|arg| {
                    let labeled = arg
                        .children_with_tokens()
                        .into_iter()
                        .any(|e| e.as_token().is_some_and(|t| t.kind() == SyntaxKind::Colon));
                    let children = arg.children();
                    let [path] = children.as_slice() else {
                        return None;
                    };
                    let path = PathExpr::cast(path.clone()).filter(|_| !labeled)?;
                    let mut segments = path.segments();
                    let name = segments.next()?;
                    segments.next().is_none().then_some(name)
                })
                .collect();
            Some((name, attr, args))
        })
        .collect()
}

/// Which way an `@after`/`@before` attribute orders a system against the
/// systems it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOrder {
    /// `@after`: the system runs after them.
    After,
    /// `@before`: the system runs before them.
    Before,
}

impl RecordDecl {
    /// The record's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The record's fields, in order.
    pub fn fields(&self) -> impl Iterator<Item = RecordField> {
        support::children(&self.syntax)
    }
}

impl RecordField {
    /// The field's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The field's declared type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The field's `=` default expression, if any.
    pub fn default(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl EnumDecl {
    /// The enum's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The enum's variants, in order.
    pub fn variants(&self) -> impl Iterator<Item = EnumVariant> {
        support::children(&self.syntax)
    }

    /// Its `@derive(..)` attributes, in source order: the attribute and the
    /// name each argument gives, `None` for an argument that is not one bare
    /// name.
    pub fn derives(&self) -> Vec<(SyntaxNode, Vec<Option<SyntaxToken>>)> {
        decl_attributes(&self.syntax)
            .into_iter()
            .filter(|(name, _, _)| name == "derive")
            .map(|(_, attr, args)| (attr, args))
            .collect()
    }
}

impl EnumVariant {
    /// The variant's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

impl InputDecl {
    /// The input's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The input's declared type (after `:`).
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The input's `=` default expression, if any.
    pub fn default(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl StateDecl {
    /// The state's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The state's declared type, when annotated (otherwise inferred).
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The state's `=` initializer expression.
    pub fn initializer(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl ComputedDecl {
    /// The computed's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The computed's declared type, when annotated.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The `=` expression the computed derives.
    pub fn body(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl EffectDecl {
    /// The effect's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The `when (..)` dependency list, when declared.
    pub fn deps(&self) -> Option<EffectDeps> {
        support::child(&self.syntax)
    }

    /// The `run Path` policy, when declared.
    pub fn run(&self) -> Option<EffectRun> {
        support::child(&self.syntax)
    }

    /// The body.
    pub fn body(&self) -> Option<EffectBody> {
        support::child(&self.syntax)
    }
}

impl ThemeDecl {
    /// The theme's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The theme it starts from.
    pub fn base(&self) -> Option<TypePath> {
        let base = self
            .syntax
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::ThemeBase)?;
        support::child(&base)
    }

    /// Its fields, in source order.
    pub fn items(&self) -> impl Iterator<Item = ThemeItem> {
        support::children(&self.syntax)
    }
}

impl ThemeItem {
    /// The field it sets.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The field's value.
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl StyleDecl {
    /// The style's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The component it styles: the type after `for`.
    pub fn target(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The styles it applies first, in order.
    pub fn bases(&self) -> Vec<TypePath> {
        self.syntax
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::StyleBases)
            .map(|bases| support::children(&bases).collect())
            .unwrap_or_default()
    }

    /// Its unconditional property bindings, in source order.
    pub fn bindings(&self) -> impl Iterator<Item = PropertyBinding> {
        support::children(&self.syntax)
    }

    /// Its `when` blocks, in source order.
    pub fn whens(&self) -> impl Iterator<Item = StyleWhen> {
        support::children(&self.syntax)
    }
}

impl StyleWhen {
    /// The selector expression.
    pub fn selector(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The bindings it applies while the selector holds.
    pub fn bindings(&self) -> impl Iterator<Item = PropertyBinding> {
        support::children(&self.syntax)
    }
}

impl ResourceDecl {
    /// The resource's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The declared `Resource<T, E>` type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The configuration items, in source order.
    pub fn items(&self) -> impl Iterator<Item = ResourceItem> {
        self.syntax
            .children()
            .into_iter()
            .filter_map(ResourceItem::cast)
    }
}

impl ResourceItem {
    /// Which item it is: [`SyntaxKind::ResourceLoad`], `ResourceKey`,
    /// `ResourcePolicy` or `ResourceScope`.
    pub fn kind(&self) -> SyntaxKind {
        self.syntax.kind()
    }

    /// The item's value.
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl EffectDeps {
    /// The dependency expressions, in order.
    pub fn exprs(&self) -> impl Iterator<Item = Expr> {
        support::children(&self.syntax)
    }
}

impl EffectRun {
    /// The policy path.
    pub fn policy(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl EffectBody {
    /// The `cleanup` clause, when present.
    pub fn cleanup(&self) -> Option<CleanupClause> {
        support::child(&self.syntax)
    }
}

impl CleanupClause {
    /// The cleanup's block.
    pub fn block(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl StartStmt {
    /// The started call.
    pub fn call(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The `as IDENT` slot, when named.
    pub fn slot(&self) -> Option<StartSlot> {
        support::child(&self.syntax)
    }

    /// The handler block, when present.
    pub fn handlers(&self) -> Option<StartHandlers> {
        support::child(&self.syntax)
    }
}

impl StartSlot {
    /// The slot's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

impl StartHandlers {
    /// The `policy = ..;` items, in order.
    pub fn policies(&self) -> impl Iterator<Item = StartPolicy> {
        support::children(&self.syntax)
    }

    /// The `success` handlers, in order.
    pub fn success(&self) -> impl Iterator<Item = StartSuccess> {
        support::children(&self.syntax)
    }

    /// The `error` handlers, in order.
    pub fn error(&self) -> impl Iterator<Item = StartError> {
        support::children(&self.syntax)
    }

    /// The `cancelled` handlers, in order.
    pub fn cancelled(&self) -> impl Iterator<Item = StartCancelled> {
        support::children(&self.syntax)
    }
}

impl StartPolicy {
    /// The policy list.
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl StartSuccess {
    /// The payload pattern.
    pub fn pattern(&self) -> Option<Pattern> {
        support::child(&self.syntax)
    }

    /// The handler's block.
    pub fn block(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl StartError {
    /// The payload pattern.
    pub fn pattern(&self) -> Option<Pattern> {
        support::child(&self.syntax)
    }

    /// The handler's block.
    pub fn block(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl StartCancelled {
    /// The handler's block.
    pub fn block(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl EventDecl {
    /// The event's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

impl SlotDecl {
    /// The slot's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The slot's type: `Slot<Node>`, `OptionalSlot<Node>` or `SlotList<Node>`.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The `= None` / `= empty` default, when written.
    pub fn default(&self) -> Option<SyntaxToken> {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .skip_while(|t| t.kind() != SyntaxKind::Eq)
            .find(|t| matches!(t.kind(), SyntaxKind::NoneKw | SyntaxKind::EmptyKw))
    }
}

impl ConstDecl {
    /// The constant's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The constant's declared type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The `=` value expression.
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl TypeAliasDecl {
    /// The alias's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

/// Shared accessors for the three callable forms (`fn`/`action`/`task`), which
/// parse to one shape under distinct kinds.
macro_rules! callable_accessors {
    ($name:ident) => {
        impl $name {
            /// The callable's name.
            pub fn name(&self) -> Option<SyntaxToken> {
                support::name_token(&self.syntax)
            }

            /// The parameter list.
            pub fn param_list(&self) -> Option<ParamList> {
                support::child(&self.syntax)
            }

            /// The parameters, in order.
            pub fn params(&self) -> Vec<Param> {
                self.param_list()
                    .map(|l| l.params().collect())
                    .unwrap_or_default()
            }

            /// The `-> Type` return type, if declared.
            pub fn return_type(&self) -> Option<ReturnType> {
                support::child(&self.syntax)
            }

            /// The body block, if the callable has one.
            pub fn body(&self) -> Option<Block> {
                support::child(&self.syntax)
            }

            /// The `requires { ... }` capability clause, if declared.
            pub fn capability_clause(&self) -> Option<CapabilityClause> {
                support::child(&self.syntax)
            }
        }
    };
}

callable_accessors!(FnDecl);
callable_accessors!(ActionDecl);
callable_accessors!(TaskDecl);
callable_accessors!(ShaderFn);

/// What a [`NativeDecl`] declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeDeclKind {
    Fn,
    Action,
    Task,
    Type,
}

impl NativeDecl {
    /// What it declares, by the keyword after `native`.
    pub fn kind(&self) -> Option<NativeDeclKind> {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find_map(|t| match t.kind() {
                SyntaxKind::FnKw => Some(NativeDeclKind::Fn),
                SyntaxKind::ActionKw => Some(NativeDeclKind::Action),
                SyntaxKind::TaskKw => Some(NativeDeclKind::Task),
                SyntaxKind::TypeKw => Some(NativeDeclKind::Type),
                _ => None,
            })
    }

    /// The declared name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// Its generic parameter list, if declared.
    pub fn generic_params(&self) -> Option<SyntaxNode> {
        self.syntax
            .children()
            .into_iter()
            .find(|n| n.kind() == SyntaxKind::GenericParams)
    }

    /// The parameter list of a callable.
    pub fn param_list(&self) -> Option<ParamList> {
        support::child(&self.syntax)
    }

    /// The parameters of a callable, in order.
    pub fn params(&self) -> Vec<Param> {
        self.param_list()
            .map(|l| l.params().collect())
            .unwrap_or_default()
    }

    /// The `-> Type` return type, if declared.
    pub fn return_type(&self) -> Option<ReturnType> {
        support::child(&self.syntax)
    }

    /// The `requires { ... }` capability clause, if declared.
    pub fn capability_clause(&self) -> Option<CapabilityClause> {
        support::child(&self.syntax)
    }
}

/// A member of a trait or an impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssocItem {
    Fn(FnDecl),
    Action(ActionDecl),
    Task(TaskDecl),
    Type(AssocTypeDecl),
    Const(ConstDecl),
}

impl AssocItem {
    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            SyntaxKind::FnDecl => AssocItem::Fn(FnDecl { syntax: node }),
            SyntaxKind::ActionDecl => AssocItem::Action(ActionDecl { syntax: node }),
            SyntaxKind::TaskDecl => AssocItem::Task(TaskDecl { syntax: node }),
            SyntaxKind::AssocTypeDecl => AssocItem::Type(AssocTypeDecl { syntax: node }),
            SyntaxKind::ConstDecl => AssocItem::Const(ConstDecl { syntax: node }),
            _ => return None,
        })
    }

    /// Its syntax node.
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            AssocItem::Fn(n) => n.syntax(),
            AssocItem::Action(n) => n.syntax(),
            AssocItem::Task(n) => n.syntax(),
            AssocItem::Type(n) => n.syntax(),
            AssocItem::Const(n) => n.syntax(),
        }
    }

    /// Its name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(self.syntax())
    }
}

/// The first child node of `node` of `kind`.
fn child_node(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().into_iter().find(|n| n.kind() == kind)
}

/// The type nodes directly under `node`.
fn type_children(node: &SyntaxNode) -> Vec<SyntaxNode> {
    node.children()
        .into_iter()
        .filter(|n| n.kind().is_type())
        .collect()
}

impl TraitDecl {
    /// The trait's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// Its generic parameter list, if declared.
    pub fn generic_params(&self) -> Option<SyntaxNode> {
        child_node(&self.syntax, SyntaxKind::GenericParams)
    }

    /// Its supertraits, the bounds after `:`.
    pub fn supertraits(&self) -> impl Iterator<Item = TypePath> {
        support::children(&self.syntax)
    }

    /// Its `where` clause, if declared.
    pub fn where_clause(&self) -> Option<SyntaxNode> {
        child_node(&self.syntax, SyntaxKind::WhereClause)
    }

    /// Its members, in order.
    pub fn members(&self) -> impl Iterator<Item = AssocItem> {
        self.syntax
            .children()
            .into_iter()
            .filter_map(AssocItem::cast)
    }
}

impl ImplDecl {
    /// Its generic parameter list, if declared.
    pub fn generic_params(&self) -> Option<SyntaxNode> {
        child_node(&self.syntax, SyntaxKind::GenericParams)
    }

    /// The trait an `impl Trait for Type` implements.
    pub fn trait_path(&self) -> Option<TypePath> {
        let types = type_children(&self.syntax);
        match &types[..] {
            [first, _] => TypePath::cast(first.clone()),
            _ => None,
        }
    }

    /// The type it adds members to: the type after `for`, or its only type.
    pub fn target(&self) -> Option<SyntaxNode> {
        type_children(&self.syntax).pop()
    }

    /// Its `where` clause, if declared.
    pub fn where_clause(&self) -> Option<SyntaxNode> {
        child_node(&self.syntax, SyntaxKind::WhereClause)
    }

    /// Its members, in order.
    pub fn members(&self) -> impl Iterator<Item = AssocItem> {
        self.syntax
            .children()
            .into_iter()
            .filter_map(AssocItem::cast)
    }
}

impl AssocTypeDecl {
    /// The associated type's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The bounds a trait's associated type declares, after `:`.
    pub fn bounds(&self) -> Vec<TypePath> {
        let Some(colon) = self
            .syntax
            .children_with_tokens()
            .into_iter()
            .position(|e| e.kind() == SyntaxKind::Colon)
        else {
            return Vec::new();
        };
        self.syntax
            .children_with_tokens()
            .into_iter()
            .skip(colon)
            .take_while(|e| e.kind() != SyntaxKind::Eq)
            .filter_map(|e| e.as_node().cloned())
            .filter_map(TypePath::cast)
            .collect()
    }

    /// The type an impl's associated type is, after `=`.
    pub fn value(&self) -> Option<SyntaxNode> {
        let eq = self
            .syntax
            .children_with_tokens()
            .into_iter()
            .position(|e| e.kind() == SyntaxKind::Eq)?;
        self.syntax
            .children_with_tokens()
            .into_iter()
            .skip(eq)
            .filter_map(|e| e.as_node().cloned())
            .find(|n| n.kind().is_type())
    }
}

impl ShaderDecl {
    /// The shader's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// Its generic parameter list, if declared.
    pub fn generic_params(&self) -> Option<SyntaxNode> {
        self.syntax
            .children()
            .into_iter()
            .find(|n| n.kind() == SyntaxKind::GenericParams)
    }

    /// Its members, in order.
    pub fn members(&self) -> impl Iterator<Item = ShaderMember> {
        support::children(&self.syntax)
    }
}

/// Shared accessors for the five shader bindings, `kw IDENT : Type ;`.
macro_rules! shader_binding_accessors {
    ($name:ident) => {
        impl $name {
            /// The binding's name.
            pub fn name(&self) -> Option<SyntaxToken> {
                support::name_token(&self.syntax)
            }

            /// The binding's type.
            pub fn ty(&self) -> Option<TypePath> {
                support::child(&self.syntax)
            }
        }
    };
}

shader_binding_accessors!(ShaderUniform);
shader_binding_accessors!(ShaderInstance);
shader_binding_accessors!(ShaderVarying);
shader_binding_accessors!(ShaderTexture);
shader_binding_accessors!(ShaderSampler);

/// The stage a shader entry point runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Vertex,
    Fragment,
    Compute,
}

impl ShaderEntry {
    /// The keyword naming its stage.
    pub fn stage_token(&self) -> Option<SyntaxToken> {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find(|t| !t.kind().is_trivia())
    }

    /// Its stage.
    pub fn stage(&self) -> Option<ShaderStage> {
        Some(match self.stage_token()?.kind() {
            SyntaxKind::VertexKw => ShaderStage::Vertex,
            SyntaxKind::FragmentKw => ShaderStage::Fragment,
            SyntaxKind::ComputeKw => ShaderStage::Compute,
            _ => return None,
        })
    }

    /// The parameter list.
    pub fn param_list(&self) -> Option<ParamList> {
        support::child(&self.syntax)
    }

    /// The parameters, in order.
    pub fn params(&self) -> Vec<Param> {
        self.param_list()
            .map(|l| l.params().collect())
            .unwrap_or_default()
    }

    /// The `-> Type` return type.
    pub fn return_type(&self) -> Option<ReturnType> {
        support::child(&self.syntax)
    }

    /// The body block.
    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

/// One member of a shader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShaderMember {
    Uniform(ShaderUniform),
    Instance(ShaderInstance),
    Varying(ShaderVarying),
    Texture(ShaderTexture),
    Sampler(ShaderSampler),
    Fn(ShaderFn),
    Entry(ShaderEntry),
}

impl AstNode for ShaderMember {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::ShaderUniform
                | SyntaxKind::ShaderInstance
                | SyntaxKind::ShaderVarying
                | SyntaxKind::ShaderTexture
                | SyntaxKind::ShaderSampler
                | SyntaxKind::ShaderFn
                | SyntaxKind::ShaderEntry
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            SyntaxKind::ShaderUniform => ShaderMember::Uniform(ShaderUniform { syntax: node }),
            SyntaxKind::ShaderInstance => ShaderMember::Instance(ShaderInstance { syntax: node }),
            SyntaxKind::ShaderVarying => ShaderMember::Varying(ShaderVarying { syntax: node }),
            SyntaxKind::ShaderTexture => ShaderMember::Texture(ShaderTexture { syntax: node }),
            SyntaxKind::ShaderSampler => ShaderMember::Sampler(ShaderSampler { syntax: node }),
            SyntaxKind::ShaderFn => ShaderMember::Fn(ShaderFn { syntax: node }),
            SyntaxKind::ShaderEntry => ShaderMember::Entry(ShaderEntry { syntax: node }),
            _ => return None,
        })
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            ShaderMember::Uniform(n) => n.syntax(),
            ShaderMember::Instance(n) => n.syntax(),
            ShaderMember::Varying(n) => n.syntax(),
            ShaderMember::Texture(n) => n.syntax(),
            ShaderMember::Sampler(n) => n.syntax(),
            ShaderMember::Fn(n) => n.syntax(),
            ShaderMember::Entry(n) => n.syntax(),
        }
    }
}

ast_node!(
    /// A `requires { CapabilityPath,* }` clause on a callable — its public capability
    /// contract / upper bound. Each capability is a plain type path.
    CapabilityClause = CapabilityClause
);

impl CapabilityClause {
    /// The declared capability paths, in source order.
    pub fn capabilities(&self) -> impl Iterator<Item = TypePath> + '_ {
        support::children(&self.syntax)
    }
}

ast_node!(
    /// A `( Param,* )` parameter list.
    ParamList = ParamList
);
ast_node!(
    /// One `mut? IDENT : Type (= Expr)?` parameter.
    Param = Param
);
ast_node!(
    /// A `-> Type` return type.
    ReturnType = ReturnType
);
ast_node!(
    /// A `TypePathSegment (:: TypePathSegment)*` type path.
    TypePath = TypePath
);
ast_node!(
    /// A `{ ... }` statement block.
    Block = Block
);

impl ParamList {
    /// The parameters, in order.
    pub fn params(&self) -> impl Iterator<Item = Param> {
        support::children(&self.syntax)
    }
}

impl Param {
    /// The parameter's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The `self` of a method's receiver parameter.
    pub fn self_token(&self) -> Option<SyntaxToken> {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find(|t| t.kind() == SyntaxKind::SelfValueKw)
    }

    /// The parameter's declared type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The `= value` default, if written.
    pub fn default(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl ReturnType {
    /// The declared return type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }
}

impl ViewDecl {
    /// The view's block of structure items.
    pub fn block(&self) -> Option<ViewBlock> {
        support::child(&self.syntax)
    }
}

// --- View --------------------------------------------------------------------

ast_node!(
    /// A `{ ViewStructureItem* }` view block.
    ViewBlock = ViewBlock
);
ast_node!(
    /// `node IDENT : ComponentType NodeBody` — a named child node.
    NamedNode = NamedNode
);
ast_node!(
    /// `ComponentType NodeBody` — an anonymous child node.
    AnonymousNode = AnonymousNode
);
ast_node!(
    /// A `{ NodeMember* }` node body.
    NodeBody = NodeBody
);
ast_node!(
    /// `PropertyPath : Expr ;` — a declarative property binding.
    PropertyBinding = PropertyBinding
);
ast_node!(
    /// An `IDENT ("." IDENT)*` property path.
    PropertyPath = PropertyPath
);
ast_node!(
    /// `bind PropertyPath <=> AssignablePath (using TypePath)? ;`.
    TwoWayBinding = TwoWayBinding
);
ast_node!(
    /// `IDENT ("." IDENT | "[" Expr "]")*` — the mutable target of a two-way bind.
    AssignablePath = AssignablePath
);
ast_node!(
    /// `on EventPhase? IDENT ( Pattern )? Block` — an event handler.
    EventHandler = EventHandler
);
ast_node!(
    /// `if HeadExpr ViewBlock (else ...)?` in view position.
    ViewIf = ViewIf
);
ast_node!(
    /// `for Pattern in HeadExpr key HeadExpr ViewBlock`.
    ViewFor = ViewFor
);
ast_node!(
    /// `match HeadExpr { ViewMatchArm,* }`.
    ViewMatch = ViewMatch
);
ast_node!(
    /// `Pattern (if Expr)? => ViewBlock` inside a view match.
    ViewMatchArm = ViewMatchArm
);
ast_node!(
    /// `fill IDENT ViewBlock`.
    FillClause = FillClause
);
ast_node!(
    /// A pattern (A.13). Every pattern and subpattern is a `Pattern` node whose
    /// single child is the production that matched (identifier, wildcard,
    /// literal, tuple, list, constructor, qualified variant, range, `@` binding
    /// or `|` alternatives).
    Pattern = Pattern
);

impl Pattern {
    /// Every name this pattern binds, in source order: identifier patterns,
    /// `name @ ..` bindings, list rests (`..rest`) and record-field shorthands
    /// (`Point { x }`). Qualified variant and constructor path segments are not
    /// bindings.
    pub fn bindings(&self) -> Vec<SyntaxToken> {
        self.syntax
            .descendants()
            .into_iter()
            .filter_map(|n| {
                let binds = match n.kind() {
                    SyntaxKind::IdentPattern
                    | SyntaxKind::BindingPattern
                    | SyntaxKind::RestPattern => true,
                    SyntaxKind::RecordPatternField => !n
                        .children_with_tokens()
                        .into_iter()
                        .any(|e| e.as_token().is_some_and(|t| t.kind() == SyntaxKind::Colon)),
                    _ => false,
                };
                binds.then(|| support::name_token(&n)).flatten()
            })
            .collect()
    }

    /// The bound name when this pattern is a single identifier pattern (`item`
    /// or `mut item`), as a `for` loop variable or an event payload binding.
    /// `None` for any other pattern shape.
    pub fn binding_name(&self) -> Option<SyntaxToken> {
        let inner = self.syntax.first_child()?;
        if inner.kind() == SyntaxKind::IdentPattern {
            support::name_token(&inner)
        } else {
            None
        }
    }
}

impl ViewBlock {
    /// The structure items directly under this block.
    pub fn items(&self) -> impl Iterator<Item = ViewItem> {
        support::children(&self.syntax)
    }
}

impl NamedNode {
    /// The node's local name (before the `:`).
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The node's component type (after the `:`).
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The node's body.
    pub fn body(&self) -> Option<NodeBody> {
        support::child(&self.syntax)
    }
}

impl AnonymousNode {
    /// The node's component type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The node's body.
    pub fn body(&self) -> Option<NodeBody> {
        support::child(&self.syntax)
    }
}

impl NodeBody {
    /// The members inside the node body, in order.
    pub fn members(&self) -> impl Iterator<Item = NodeMember> {
        support::children(&self.syntax)
    }
}

impl PropertyBinding {
    /// The bound property path (the left side, before the `:`).
    pub fn path(&self) -> Option<PropertyPath> {
        support::child(&self.syntax)
    }

    /// The bound value expression (the right side, after the `:`).
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl PropertyPath {
    /// The path's dotted segments, in order.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
    }
}

impl EventHandler {
    /// The optional capture/bubble phase marker preceding the event name.
    pub fn phase(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |k| {
            matches!(k, SyntaxKind::CaptureKw | SyntaxKind::BubbleKw)
        })
    }

    /// The event name this handler binds.
    pub fn event(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The optional `( Pattern )` payload binding.
    pub fn payload(&self) -> Option<Pattern> {
        support::child(&self.syntax)
    }

    /// The handler's block body.
    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl TwoWayBinding {
    /// The bound property path (the left side, before `<=>`).
    pub fn target(&self) -> Option<PropertyPath> {
        support::child(&self.syntax)
    }

    /// The mutable source path (the right side, after `<=>`).
    pub fn source(&self) -> Option<AssignablePath> {
        support::child(&self.syntax)
    }

    /// The optional `using TypePath` coercion type.
    pub fn using_ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }
}

impl AssignablePath {
    /// The path's leading name and dotted field segments, in order. Index
    /// suffixes (`[ Expr ]`) carry their own expression child and are not names.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
    }
}

impl FillClause {
    /// The name of the slot this clause fills.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The projected content block.
    pub fn body(&self) -> Option<ViewBlock> {
        support::child(&self.syntax)
    }
}

impl ViewIf {
    /// The condition head expression.
    pub fn condition(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The optional `preserve "..."` identity string literal.
    pub fn preserve(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |k| {
            matches!(k, SyntaxKind::StringLiteral | SyntaxKind::RawStringLiteral)
        })
    }

    /// The `preserve` identity as the literal's body is written, without its
    /// delimiters: two branches naming the same body share one identity.
    pub fn preserve_name(&self) -> Option<String> {
        let token = self.preserve()?;
        let text = token.text().to_string();
        let body = match token.kind() {
            SyntaxKind::RawStringLiteral => text.trim_start_matches('r').trim_matches('#'),
            _ => text.as_str(),
        };
        Some(body.strip_prefix('"')?.strip_suffix('"')?.to_string())
    }

    /// The `then` view block (the first block child).
    pub fn then_block(&self) -> Option<ViewBlock> {
        support::child(&self.syntax)
    }

    /// The `else` branch: either a chained `else if` ([`ElseBranch::If`]) or a
    /// trailing `else` block ([`ElseBranch::Block`]).
    pub fn else_branch(&self) -> Option<ElseBranch> {
        // A chained `else if` nests a `ViewIf`; a plain `else` adds a second
        // `ViewBlock`. Prefer the nested `ViewIf`, else the block after the first.
        if let Some(nested) = support::child::<ViewIf>(&self.syntax) {
            return Some(ElseBranch::If(nested));
        }
        support::nth_child::<ViewBlock>(&self.syntax, 1).map(ElseBranch::Block)
    }
}

/// The `else` branch of a [`ViewIf`]: a chained `else if` or a trailing block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElseBranch {
    If(ViewIf),
    Block(ViewBlock),
}

impl ViewFor {
    /// The loop pattern (the binding introduced for each item).
    pub fn pattern(&self) -> Option<Pattern> {
        support::child(&self.syntax)
    }

    /// The iterable head expression (the first head expression).
    pub fn iterable(&self) -> Option<Expr> {
        support::nth_child(&self.syntax, 0)
    }

    /// The stable-key head expression (the second head expression).
    pub fn key(&self) -> Option<Expr> {
        support::nth_child(&self.syntax, 1)
    }

    /// The loop body's view block.
    pub fn body(&self) -> Option<ViewBlock> {
        support::child(&self.syntax)
    }
}

impl ViewMatch {
    /// The scrutinee head expression.
    pub fn scrutinee(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The match arms, in order.
    pub fn arms(&self) -> impl Iterator<Item = ViewMatchArm> {
        support::children(&self.syntax)
    }
}

impl ViewMatchArm {
    /// The arm's pattern.
    pub fn pattern(&self) -> Option<Pattern> {
        support::child(&self.syntax)
    }

    /// The optional `if Expr` guard.
    pub fn guard(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The arm's view block body.
    pub fn body(&self) -> Option<ViewBlock> {
        support::child(&self.syntax)
    }
}

// --- Expressions -------------------------------------------------------------

ast_node!(
    /// A literal expression (int/float/string/color/bool/…).
    LiteralExpr = LiteralExpr
);
ast_node!(
    /// A path expression (`a`, `a::b`, `self`, `Self`).
    PathExpr = PathExpr
);
ast_node!(
    /// A binary operator expression `lhs op rhs`.
    BinaryExpr = BinaryExpr
);
ast_node!(
    /// A prefix unary expression `op expr`.
    UnaryExpr = UnaryExpr
);
ast_node!(
    /// A `callee GenericCallArgs? ( args )` call.
    CallExpr = CallExpr
);
ast_node!(
    /// An `expr [ index ]` index.
    IndexExpr = IndexExpr
);
ast_node!(
    /// An `expr . IDENT` field access.
    FieldExpr = FieldExpr
);
ast_node!(
    /// An `expr ?` try.
    TryExpr = TryExpr
);
ast_node!(
    /// A `lo (.. | ..=) hi` range.
    RangeExpr = RangeExpr
);
ast_node!(
    /// An `expr as Type` cast.
    CastExpr = CastExpr
);
ast_node!(
    /// A `Path? { RecordExprField,* }` record expression.
    RecordExpr = RecordExpr
);
ast_node!(
    /// A `( Expr,* )` tuple.
    TupleExpr = TupleExpr
);
ast_node!(
    /// A `[ Expr,* ]` list.
    ListExpr = ListExpr
);
ast_node!(
    /// A `( Expr )` parenthesized expression.
    ParenExpr = ParenExpr
);
ast_node!(
    /// An `if ... else ...` expression.
    IfExpr = IfExpr
);
ast_node!(
    /// A `match ... { arm,* }` expression.
    MatchExpr = MatchExpr
);
ast_node!(
    /// A closure `move? |params| body`.
    ClosureExpr = ClosureExpr
);

impl LiteralExpr {
    /// The single literal token this expression wraps (int/float/unit/string/
    /// char/color/bool/none).
    pub fn token(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |k| {
            matches!(
                k,
                SyntaxKind::IntLiteral
                    | SyntaxKind::FloatLiteral
                    | SyntaxKind::UnitLiteral
                    | SyntaxKind::StringLiteral
                    | SyntaxKind::RawStringLiteral
                    | SyntaxKind::CharLiteral
                    | SyntaxKind::ColorLiteral
                    | SyntaxKind::TrueKw
                    | SyntaxKind::FalseKw
                    | SyntaxKind::NoneKw
            )
        })
    }
}

/// Whether a token kind is one of the infix binary operators the parser places
/// bare between a `BinaryExpr`'s two operand children.
fn is_binary_op(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Star
            | SyntaxKind::Slash
            | SyntaxKind::Percent
            | SyntaxKind::Amp
            | SyntaxKind::Pipe
            | SyntaxKind::Caret
            | SyntaxKind::Shl
            | SyntaxKind::Shr
            | SyntaxKind::EqEq
            | SyntaxKind::Neq
            | SyntaxKind::Lt
            | SyntaxKind::Le
            | SyntaxKind::Gt
            | SyntaxKind::Ge
            | SyntaxKind::AmpAmp
            | SyntaxKind::PipePipe
            | SyntaxKind::QuestionQuestion
    )
}

impl BinaryExpr {
    /// The left operand (the first expression child).
    pub fn lhs(&self) -> Option<Expr> {
        support::nth_child(&self.syntax, 0)
    }

    /// The operator token between the operands.
    pub fn op(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, is_binary_op)
    }

    /// The right operand (the second expression child).
    pub fn rhs(&self) -> Option<Expr> {
        support::nth_child(&self.syntax, 1)
    }
}

impl UnaryExpr {
    /// The prefix operator token (`-`, `+`, `!`, `~`, `await`).
    pub fn op(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |k| {
            matches!(
                k,
                SyntaxKind::Minus
                    | SyntaxKind::Plus
                    | SyntaxKind::Bang
                    | SyntaxKind::Tilde
                    | SyntaxKind::AwaitKw
            )
        })
    }

    /// The operand expression.
    pub fn operand(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl ParenExpr {
    /// The parenthesized expression.
    pub fn inner(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl FieldExpr {
    /// The receiver expression (`a` in `a.b`).
    pub fn receiver(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The accessed field name.
    pub fn field(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

impl CallExpr {
    /// The callee expression.
    pub fn callee(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl CastExpr {
    /// The operand being cast.
    pub fn operand(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The target type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }
}

impl PathExpr {
    /// The path's identifier segments, root first. A `PathExpr` holds its
    /// `IDENT ("::" IDENT)*` sequence as bare tokens, so the segments are the
    /// direct identifier children.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| {
                matches!(
                    t.kind(),
                    SyntaxKind::Ident
                        | SyntaxKind::RawIdent
                        | SyntaxKind::SelfValueKw
                        | SyntaxKind::SelfTypeKw
                )
            })
    }
}

impl TypePath {
    /// The path's segment names, root first. Each `TypePathSegment` child owns a
    /// leading name token; this projects that name, skipping generic arguments.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children()
            .into_iter()
            .filter(|n| n.kind() == SyntaxKind::TypePathSegment)
            .filter_map(|seg| {
                support::name_token(&seg).or_else(|| {
                    seg.children_with_tokens()
                        .into_iter()
                        .filter_map(|e| e.as_token().cloned())
                        .find(|t| t.kind() == SyntaxKind::SelfTypeKw)
                })
            })
    }
}

// --- Enum wrappers -----------------------------------------------------------

/// Any expression node. The catch-all typed view over the expression grammar;
/// [`Expr::cast`] accepts every expression node kind and [`Expr::syntax`] returns
/// the concrete node for further casting to a specific wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    syntax: SyntaxNode,
}

impl Expr {
    /// Whether `kind` is one of the expression node kinds.
    fn is_expr_kind(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::LiteralExpr
                | SyntaxKind::PathExpr
                | SyntaxKind::BinaryExpr
                | SyntaxKind::UnaryExpr
                | SyntaxKind::CallExpr
                | SyntaxKind::IndexExpr
                | SyntaxKind::FieldExpr
                | SyntaxKind::OptionalFieldExpr
                | SyntaxKind::TryExpr
                | SyntaxKind::RangeExpr
                | SyntaxKind::CastExpr
                | SyntaxKind::RecordExpr
                | SyntaxKind::TupleExpr
                | SyntaxKind::ListExpr
                | SyntaxKind::ParenExpr
                | SyntaxKind::IfExpr
                | SyntaxKind::MatchExpr
                | SyntaxKind::ClosureExpr
                | SyntaxKind::BlockExpr
        )
    }
}

impl AstNode for Expr {
    fn can_cast(kind: SyntaxKind) -> bool {
        Self::is_expr_kind(kind)
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        if Self::can_cast(node.kind()) {
            Some(Expr { syntax: node })
        } else {
            None
        }
    }
    fn syntax(&self) -> &SyntaxNode {
        &self.syntax
    }
}

/// A top-level declaration in a compilation unit: a declaration, optionally
/// wrapped in `export`, or an Advanced placeholder.
///
/// Imports are *not* items — they form their own head list, reached through
/// [`CompilationUnit::imports`], so [`CompilationUnit::items`] projects only the
/// declaration tail. An `export`-prefixed declaration appears as [`Item::Export`];
/// the wrapped declaration is reached through [`ExportDecl::declaration`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Export(ExportDecl),
    Component(ComponentDecl),
    System(SystemDecl),
    Record(RecordDecl),
    Enum(EnumDecl),
    Const(ConstDecl),
    TypeAlias(TypeAliasDecl),
    Fn(FnDecl),
    Action(ActionDecl),
    Task(TaskDecl),
    Shader(ShaderDecl),
    Theme(ThemeDecl),
    Style(StyleDecl),
    Native(NativeDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    Template(TemplateDecl),
}

impl AstNode for Item {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::ExportDecl
                | SyntaxKind::ComponentDecl
                | SyntaxKind::SystemDecl
                | SyntaxKind::RecordDecl
                | SyntaxKind::EnumDecl
                | SyntaxKind::ConstDecl
                | SyntaxKind::TypeAliasDecl
                | SyntaxKind::FnDecl
                | SyntaxKind::ActionDecl
                | SyntaxKind::TaskDecl
                | SyntaxKind::ShaderDecl
                | SyntaxKind::ThemeDecl
                | SyntaxKind::StyleDecl
                | SyntaxKind::NativeDecl
                | SyntaxKind::TraitDecl
                | SyntaxKind::ImplDecl
                | SyntaxKind::TemplateDecl
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        let item = match node.kind() {
            SyntaxKind::ExportDecl => Item::Export(ExportDecl { syntax: node }),
            SyntaxKind::ComponentDecl => Item::Component(ComponentDecl { syntax: node }),
            SyntaxKind::SystemDecl => Item::System(SystemDecl { syntax: node }),
            SyntaxKind::RecordDecl => Item::Record(RecordDecl { syntax: node }),
            SyntaxKind::EnumDecl => Item::Enum(EnumDecl { syntax: node }),
            SyntaxKind::ConstDecl => Item::Const(ConstDecl { syntax: node }),
            SyntaxKind::TypeAliasDecl => Item::TypeAlias(TypeAliasDecl { syntax: node }),
            SyntaxKind::FnDecl => Item::Fn(FnDecl { syntax: node }),
            SyntaxKind::ActionDecl => Item::Action(ActionDecl { syntax: node }),
            SyntaxKind::TaskDecl => Item::Task(TaskDecl { syntax: node }),
            SyntaxKind::ShaderDecl => Item::Shader(ShaderDecl { syntax: node }),
            SyntaxKind::ThemeDecl => Item::Theme(ThemeDecl { syntax: node }),
            SyntaxKind::StyleDecl => Item::Style(StyleDecl { syntax: node }),
            SyntaxKind::NativeDecl => Item::Native(NativeDecl { syntax: node }),
            SyntaxKind::TraitDecl => Item::Trait(TraitDecl { syntax: node }),
            SyntaxKind::ImplDecl => Item::Impl(ImplDecl { syntax: node }),
            SyntaxKind::TemplateDecl => Item::Template(TemplateDecl { syntax: node }),
            _ => return None,
        };
        Some(item)
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Item::Export(n) => n.syntax(),
            Item::Component(n) => n.syntax(),
            Item::System(n) => n.syntax(),
            Item::Record(n) => n.syntax(),
            Item::Enum(n) => n.syntax(),
            Item::Const(n) => n.syntax(),
            Item::TypeAlias(n) => n.syntax(),
            Item::Fn(n) => n.syntax(),
            Item::Action(n) => n.syntax(),
            Item::Task(n) => n.syntax(),
            Item::Shader(n) => n.syntax(),
            Item::Theme(n) => n.syntax(),
            Item::Style(n) => n.syntax(),
            Item::Native(n) => n.syntax(),
            Item::Trait(n) => n.syntax(),
            Item::Impl(n) => n.syntax(),
            Item::Template(n) => n.syntax(),
        }
    }
}

/// A member of a `component`/`system` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member {
    Input(InputDecl),
    State(StateDecl),
    Computed(ComputedDecl),
    Event(EventDecl),
    Slot(SlotDecl),
    Fn(FnDecl),
    Action(ActionDecl),
    Task(TaskDecl),
    Effect(EffectDecl),
    Resource(ResourceDecl),
    View(ViewDecl),
    Native(NativeDecl),
    Const(ConstDecl),
}

impl AstNode for Member {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::InputDecl
                | SyntaxKind::StateDecl
                | SyntaxKind::ComputedDecl
                | SyntaxKind::EventDecl
                | SyntaxKind::SlotDecl
                | SyntaxKind::FnDecl
                | SyntaxKind::ActionDecl
                | SyntaxKind::TaskDecl
                | SyntaxKind::EffectDecl
                | SyntaxKind::ResourceDecl
                | SyntaxKind::ViewDecl
                | SyntaxKind::NativeDecl
                | SyntaxKind::ConstDecl
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        let member = match node.kind() {
            SyntaxKind::InputDecl => Member::Input(InputDecl { syntax: node }),
            SyntaxKind::StateDecl => Member::State(StateDecl { syntax: node }),
            SyntaxKind::ComputedDecl => Member::Computed(ComputedDecl { syntax: node }),
            SyntaxKind::EventDecl => Member::Event(EventDecl { syntax: node }),
            SyntaxKind::SlotDecl => Member::Slot(SlotDecl { syntax: node }),
            SyntaxKind::FnDecl => Member::Fn(FnDecl { syntax: node }),
            SyntaxKind::ActionDecl => Member::Action(ActionDecl { syntax: node }),
            SyntaxKind::TaskDecl => Member::Task(TaskDecl { syntax: node }),
            SyntaxKind::EffectDecl => Member::Effect(EffectDecl { syntax: node }),
            SyntaxKind::ResourceDecl => Member::Resource(ResourceDecl { syntax: node }),
            SyntaxKind::ViewDecl => Member::View(ViewDecl { syntax: node }),
            SyntaxKind::NativeDecl => Member::Native(NativeDecl { syntax: node }),
            SyntaxKind::ConstDecl => Member::Const(ConstDecl { syntax: node }),
            _ => return None,
        };
        Some(member)
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Member::Input(n) => n.syntax(),
            Member::State(n) => n.syntax(),
            Member::Computed(n) => n.syntax(),
            Member::Event(n) => n.syntax(),
            Member::Slot(n) => n.syntax(),
            Member::Fn(n) => n.syntax(),
            Member::Action(n) => n.syntax(),
            Member::Task(n) => n.syntax(),
            Member::Effect(n) => n.syntax(),
            Member::Resource(n) => n.syntax(),
            Member::View(n) => n.syntax(),
            Member::Native(n) => n.syntax(),
            Member::Const(n) => n.syntax(),
        }
    }
}

/// A structure item inside a `view` block or `ui!` fragment: a node, a property,
/// a handler, a binding, or a control-flow form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewItem {
    Named(NamedNode),
    Anonymous(AnonymousNode),
    Property(PropertyBinding),
    Handler(EventHandler),
    TwoWayBinding(TwoWayBinding),
    If(ViewIf),
    For(ViewFor),
    Match(ViewMatch),
    Fill(FillClause),
    Part(PartNode),
    Use(TemplateUse),
    Override(PartOverride),
    Replace(PartReplace),
}

impl AstNode for ViewItem {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::NamedNode
                | SyntaxKind::AnonymousNode
                | SyntaxKind::PropertyBinding
                | SyntaxKind::EventHandler
                | SyntaxKind::TwoWayBinding
                | SyntaxKind::ViewIf
                | SyntaxKind::ViewFor
                | SyntaxKind::ViewMatch
                | SyntaxKind::FillClause
                | SyntaxKind::PartNode
                | SyntaxKind::TemplateUse
                | SyntaxKind::PartOverride
                | SyntaxKind::PartReplace
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        let item = match node.kind() {
            SyntaxKind::NamedNode => ViewItem::Named(NamedNode { syntax: node }),
            SyntaxKind::AnonymousNode => ViewItem::Anonymous(AnonymousNode { syntax: node }),
            SyntaxKind::PropertyBinding => ViewItem::Property(PropertyBinding { syntax: node }),
            SyntaxKind::EventHandler => ViewItem::Handler(EventHandler { syntax: node }),
            SyntaxKind::TwoWayBinding => ViewItem::TwoWayBinding(TwoWayBinding { syntax: node }),
            SyntaxKind::ViewIf => ViewItem::If(ViewIf { syntax: node }),
            SyntaxKind::ViewFor => ViewItem::For(ViewFor { syntax: node }),
            SyntaxKind::ViewMatch => ViewItem::Match(ViewMatch { syntax: node }),
            SyntaxKind::FillClause => ViewItem::Fill(FillClause { syntax: node }),
            SyntaxKind::PartNode => ViewItem::Part(PartNode { syntax: node }),
            SyntaxKind::TemplateUse => ViewItem::Use(TemplateUse { syntax: node }),
            SyntaxKind::PartOverride => ViewItem::Override(PartOverride { syntax: node }),
            SyntaxKind::PartReplace => ViewItem::Replace(PartReplace { syntax: node }),
            _ => return None,
        };
        Some(item)
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            ViewItem::Named(n) => n.syntax(),
            ViewItem::Anonymous(n) => n.syntax(),
            ViewItem::Property(n) => n.syntax(),
            ViewItem::Handler(n) => n.syntax(),
            ViewItem::TwoWayBinding(n) => n.syntax(),
            ViewItem::If(n) => n.syntax(),
            ViewItem::For(n) => n.syntax(),
            ViewItem::Match(n) => n.syntax(),
            ViewItem::Fill(n) => n.syntax(),
            ViewItem::Part(n) => n.syntax(),
            ViewItem::Use(n) => n.syntax(),
            ViewItem::Override(n) => n.syntax(),
            ViewItem::Replace(n) => n.syntax(),
        }
    }
}

/// A member of a `node` body: the same view structure items as a view block plus
/// property bindings apply, so a node body reuses [`ViewItem`].
pub type NodeMember = ViewItem;
