//! The declaration grammar (Appendix A.2, A.4–A.7). A compilation unit is a run
//! of imports followed by top-level declarations; a component/system body is a run
//! of typed members. This is the declarative surface of the `:` vs `=` split — a
//! `:` separates a name from its type (`input x: T`, `field: T`, `name: Type`),
//! while `=` gives an initializer or default (`state x = e`, `const C: T = e`).
//!
//! ## Core versus Advanced
//!
//! Only the Core, the Shader Profile's `shader`, and a few Standard forms get
//! dedicated node kinds and later resolution. Advanced declarations
//! (`trait`/`impl`/`template`/`style`/`theme`/`native`, and the Standard
//! `effect`/`resource`) are parsed just enough
//! to consume their body — their brace group is skipped as a balanced run — and
//! wrapped in a single [`SyntaxKind::AdvancedItem`] so they neither break the tree
//! nor gate the slice. Their resolution lands when their consumer does.

use super::super::kind::SyntaxKind;
use super::{ParseErrorKind, Parser, attributes, label, name};

/// `ImportDecl* TopLevelDecl* EOF` — the body of a `.vs` file / `view!` entry.
pub(super) fn compilation_unit(p: &mut Parser) {
    while !p.at_end() {
        let before = p.cursor();
        compilation_unit_item(p);
        p.ensure_progress(before);
    }
}

/// One item of a compilation unit: an import or a top-level declaration.
pub(super) fn compilation_unit_item(p: &mut Parser) {
    if p.at(SyntaxKind::ImportKw) {
        import_decl(p);
    } else {
        top_level_decl(p);
    }
}

/// `ImportDecl* "component"? ComponentDecl EOF` — the body of a `component!`
/// entry. Imports may precede the single component, whose `component` keyword is
/// optional (`component! { Counter { … } }`). Anything after it is reported and
/// still parsed as top-level declarations, so a malformed entry keeps its shape.
pub(super) fn component_entry(p: &mut Parser) {
    while p.at(SyntaxKind::ImportKw) {
        import_decl(p);
    }
    attributes(p);
    match p.current() {
        SyntaxKind::ComponentKw => component_decl(p),
        SyntaxKind::Ident | SyntaxKind::RawIdent => component_body(p),
        _ => p.error(ParseErrorKind::MissingToken),
    }
    if !p.at_end() {
        p.error(ParseErrorKind::UnexpectedTokens);
        while !p.at_end() {
            let before = p.cursor();
            top_level_decl(p);
            p.ensure_progress(before);
        }
    }
}

/// `"import" ModulePath ImportSuffix? ";"` where the suffix is `"as" IDENT` or
/// `"::" "{" ImportItem ("," ImportItem)* ","? "}"`.
fn import_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `import`
    module_path(p);
    match p.current() {
        SyntaxKind::AsKw => {
            let r = p.start();
            p.bump_any(); // `as`
            name(p);
            r.complete(p, SyntaxKind::RenameClause);
        }
        SyntaxKind::ColonColon => {
            p.bump_any(); // `::`
            p.expect(SyntaxKind::LBrace);
            while !p.at(SyntaxKind::RBrace) && !p.at_end() {
                import_item(p);
                if !p.eat(SyntaxKind::Comma) {
                    break;
                }
            }
            p.expect(SyntaxKind::RBrace);
        }
        _ => {}
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ImportDecl);
}

/// `IDENT ("::" Label)*` — a module path.
fn module_path(p: &mut Parser) {
    let m = p.start();
    name(p);
    while p.at(SyntaxKind::ColonColon) && p.nth(1) != SyntaxKind::LBrace {
        p.bump_any(); // `::`
        label(p);
    }
    m.complete(p, SyntaxKind::ModulePath);
}

/// `IDENT ("as" IDENT)?` — one item in a selective import list.
fn import_item(p: &mut Parser) {
    let m = p.start();
    name(p);
    if p.at(SyntaxKind::AsKw) {
        let r = p.start();
        p.bump_any(); // `as`
        name(p);
        r.complete(p, SyntaxKind::RenameClause);
    }
    m.complete(p, SyntaxKind::ImportItem);
}

/// `Attribute* "export"? DeclCore` — one top-level declaration. Leading attributes
/// and an `export` prefix are consumed into the declaration's own node so a later
/// pass reads them from one place.
pub(super) fn top_level_decl(p: &mut Parser) {
    attributes(p);
    let exported = p.at(SyntaxKind::ExportKw);
    if exported {
        // An exported item is wrapped so visibility travels with the declaration.
        let m = p.start();
        p.bump_any(); // `export`
        decl_core(p);
        m.complete(p, SyntaxKind::ExportDecl);
    } else {
        decl_core(p);
    }
}

/// One declaration core: dispatched on the leading keyword. Core and a few
/// Standard forms get real nodes; everything else becomes an advanced item.
fn decl_core(p: &mut Parser) {
    match p.current() {
        SyntaxKind::ComponentKw => component_decl(p),
        SyntaxKind::SystemKw => system_decl(p),
        SyntaxKind::RecordKw => record_decl(p),
        SyntaxKind::EnumKw => enum_decl(p),
        SyntaxKind::TypeKw => type_alias_decl(p),
        SyntaxKind::ConstKw => const_decl(p),
        SyntaxKind::FnKw => fn_like_decl(p, SyntaxKind::FnDecl),
        SyntaxKind::ActionKw => fn_like_decl(p, SyntaxKind::ActionDecl),
        SyntaxKind::TaskKw => fn_like_decl(p, SyntaxKind::TaskDecl),
        SyntaxKind::ShaderKw => shader_decl(p),
        // Standard/Advanced declarations parsed but not resolved this slice.
        SyntaxKind::TraitKw | SyntaxKind::ImplKw | SyntaxKind::NativeKw => advanced_decl(p, None),
        SyntaxKind::Ident if p.nth_is_ident(1) => match p.nth_contextual(0) {
            Some(kw @ (SyntaxKind::TemplateKw | SyntaxKind::StyleKw | SyntaxKind::ThemeKw)) => {
                advanced_decl(p, Some(kw))
            }
            _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
        },
        _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
    }
}

/// `"component" IDENT GenericParams? ImplementsClause? WhereClause? "{"
/// ComponentMember* "}"`.
fn component_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `component`
    component_rest(p);
    m.complete(p, SyntaxKind::ComponentDecl);
}

/// A `component!` component written without its keyword: `IDENT … { member* }`.
fn component_body(p: &mut Parser) {
    let m = p.start();
    component_rest(p);
    m.complete(p, SyntaxKind::ComponentDecl);
}

/// Everything in a component declaration after the keyword.
fn component_rest(p: &mut Parser) {
    name(p);
    generic_params(p);
    implements_clause(p);
    where_clause(p);
    member_block(p, member);
}

/// `"system" IDENT GenericParams? ImplementsClause? WhereClause? "{"
/// SystemMember* "}"`. Members are the same as a component's minus `event`/`slot`/
/// `view`; the parser accepts the shared set and leaves that restriction to HIR.
fn system_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `system`
    name(p);
    generic_params(p);
    implements_clause(p);
    where_clause(p);
    member_block(p, member);
    m.complete(p, SyntaxKind::SystemDecl);
}

/// `"{" Member* "}"` — a component/system body, each member parsed by `f`.
fn member_block(p: &mut Parser, f: fn(&mut Parser)) {
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        f(p);
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
}

/// One component/system member: dispatched on its leading keyword.
pub(super) fn member(p: &mut Parser) {
    attributes(p);
    // A member keyword is recognized before a name; a strict keyword there is
    // still that member (its name then reports E1301).
    let contextual = if p.nth_is_ident(1) || p.nth(1).is_keyword() {
        p.nth_contextual(0)
    } else {
        None
    };
    match contextual {
        Some(SyntaxKind::InputKw) => return input_decl(p),
        Some(SyntaxKind::StateKw) => return state_decl(p),
        Some(SyntaxKind::ComputedKw) => return computed_decl(p),
        Some(SyntaxKind::EventKw) => return event_decl(p),
        Some(SyntaxKind::SlotKw) => return slot_decl(p),
        Some(kw @ (SyntaxKind::EffectKw | SyntaxKind::ResourceKw)) => {
            return advanced_decl(p, Some(kw));
        }
        _ => {}
    }
    match p.current() {
        SyntaxKind::Ident
            if p.at_contextual(SyntaxKind::ViewKw) && p.nth(1) == SyntaxKind::LBrace =>
        {
            super::view::view_decl(p)
        }
        SyntaxKind::ConstKw => const_decl(p),
        SyntaxKind::FnKw => fn_like_decl(p, SyntaxKind::FnDecl),
        SyntaxKind::ActionKw => fn_like_decl(p, SyntaxKind::ActionDecl),
        SyntaxKind::TaskKw => fn_like_decl(p, SyntaxKind::TaskDecl),
        SyntaxKind::NativeKw => advanced_decl(p, None),
        _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
    }
}

/// `"shader" IDENT GenericParams? "{" ShaderMember* "}"` (§97).
fn shader_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `shader`
    name(p);
    generic_params(p);
    member_block(p, shader_member);
    m.complete(p, SyntaxKind::ShaderDecl);
}

/// One shader member: a `uniform`/`instance`/`varying`/`texture`/`sampler`
/// binding, a `fn`, or a `vertex`/`fragment`/`compute` entry point.
fn shader_member(p: &mut Parser) {
    attributes(p);
    let named = p.nth_is_ident(1) || p.nth(1).is_keyword();
    match p.nth_contextual(0) {
        Some(kw) if named => {
            let kind = match kw {
                SyntaxKind::UniformKw => SyntaxKind::ShaderUniform,
                SyntaxKind::InstanceKw => SyntaxKind::ShaderInstance,
                SyntaxKind::VaryingKw => SyntaxKind::ShaderVarying,
                SyntaxKind::TextureKw => SyntaxKind::ShaderTexture,
                SyntaxKind::SamplerKw => SyntaxKind::ShaderSampler,
                _ => return p.err_and_bump(ParseErrorKind::UnexpectedTokens),
            };
            let m = p.start();
            p.bump_as(kw);
            name(p);
            p.expect(SyntaxKind::Colon);
            super::types::type_(p);
            p.expect(SyntaxKind::Semi);
            m.complete(p, kind);
        }
        Some(kw @ (SyntaxKind::VertexKw | SyntaxKind::FragmentKw | SyntaxKind::ComputeKw))
            if p.nth(1) == SyntaxKind::LParen =>
        {
            let m = p.start();
            p.bump_as(kw);
            param_list(p);
            return_type(p);
            super::stmt::block(p);
            m.complete(p, SyntaxKind::ShaderEntry);
        }
        _ if p.at(SyntaxKind::FnKw) => fn_like_decl(p, SyntaxKind::ShaderFn),
        _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
    }
}

/// `"input" IDENT ":" Type ("=" DefaultExpression)? ";"`.
fn input_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::InputKw);
    name(p);
    p.expect(SyntaxKind::Colon);
    super::types::type_(p);
    if p.eat(SyntaxKind::Eq) {
        super::expr::expr(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::InputDecl);
}

/// `"state" IDENT (":" Type)? "=" InitExpression ";"` — the type may be inferred
/// from the initializer, so only the `=` initializer is required.
fn state_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::StateKw);
    name(p);
    if p.eat(SyntaxKind::Colon) {
        super::types::type_(p);
    }
    // A missing `=` still parses the value that follows, so recovery reports
    // only the absent token; an `=` with no value is a missing expression.
    let eq = p.eat(SyntaxKind::Eq);
    if !eq {
        p.error(ParseErrorKind::MissingToken);
    }
    if eq || !p.at(SyntaxKind::Semi) {
        super::expr::expr(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::StateDecl);
}

/// `"computed" IDENT (":" Type)? "=" Expression ";"`.
fn computed_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::ComputedKw);
    name(p);
    if p.eat(SyntaxKind::Colon) {
        super::types::type_(p);
    }
    // A missing `=` still parses the value that follows, so recovery reports
    // only the absent token; an `=` with no value is a missing expression.
    let eq = p.eat(SyntaxKind::Eq);
    if !eq {
        p.error(ParseErrorKind::MissingToken);
    }
    if eq || !p.at(SyntaxKind::Semi) {
        super::expr::expr(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ComputedDecl);
}

/// `"event" IDENT "(" EventParameterList ")" ";"`.
fn event_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::EventKw);
    name(p);
    p.expect(SyntaxKind::LParen);
    while !p.at(SyntaxKind::RParen) && !p.at_end() {
        event_param(p);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::RParen);
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::EventDecl);
}

/// `IDENT ":" Type` — one event parameter.
fn event_param(p: &mut Parser) {
    let m = p.start();
    name(p);
    p.expect(SyntaxKind::Colon);
    super::types::type_(p);
    m.complete(p, SyntaxKind::EventParam);
}

/// `"slot" IDENT ":" Type ("=" SlotDefault)? ";"` where the default is `None` or
/// the context word `empty`.
fn slot_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_as(SyntaxKind::SlotKw);
    name(p);
    p.expect(SyntaxKind::Colon);
    super::types::type_(p);
    if p.eat(SyntaxKind::Eq) {
        // The default is `None` or the `empty` context word; consume either.
        if p.at(SyntaxKind::NoneKw) || (p.at(SyntaxKind::Ident) && p.token_text(0) == "empty") {
            p.bump_any();
        } else {
            p.error(ParseErrorKind::MissingToken);
        }
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::SlotDecl);
}

/// `"record" IDENT GenericParams? ImplementsClause? WhereClause? "{" RecordField*
/// "}"`.
fn record_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `record`
    name(p);
    generic_params(p);
    implements_clause(p);
    where_clause(p);
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        record_field(p);
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::RecordDecl);
}

/// `Attribute* Label ":" Type ("=" ConstExpression)? ";"` — one record field.
fn record_field(p: &mut Parser) {
    let m = p.start();
    attributes(p);
    label(p);
    p.expect(SyntaxKind::Colon);
    super::types::type_(p);
    if p.eat(SyntaxKind::Eq) {
        super::expr::expr(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::RecordField);
}

/// `"enum" IDENT GenericParams? ImplementsClause? WhereClause? "{" EnumVariant*
/// "}"`.
fn enum_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `enum`
    name(p);
    generic_params(p);
    implements_clause(p);
    where_clause(p);
    p.expect(SyntaxKind::LBrace);
    while !p.at(SyntaxKind::RBrace) && !p.at_end() {
        let before = p.cursor();
        enum_variant(p);
        p.ensure_progress(before);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::EnumDecl);
}

/// `Attribute* Label VariantPayload? ";"` where a payload is a tuple `"(" TypeList
/// ")"` or a record `"{" RecordField* "}"`.
fn enum_variant(p: &mut Parser) {
    let m = p.start();
    attributes(p);
    label(p);
    match p.current() {
        SyntaxKind::LParen => {
            let pay = p.start();
            p.bump_any(); // `(`
            while !p.at(SyntaxKind::RParen) && !p.at_end() {
                super::types::type_(p);
                if !p.eat(SyntaxKind::Comma) {
                    break;
                }
            }
            p.expect(SyntaxKind::RParen);
            pay.complete(p, SyntaxKind::VariantPayload);
        }
        SyntaxKind::LBrace => {
            let pay = p.start();
            p.bump_any(); // `{`
            while !p.at(SyntaxKind::RBrace) && !p.at_end() {
                let before = p.cursor();
                record_field(p);
                p.ensure_progress(before);
            }
            p.expect(SyntaxKind::RBrace);
            pay.complete(p, SyntaxKind::VariantPayload);
        }
        _ => {}
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::EnumVariant);
}

/// `"type" IDENT GenericParams? "=" Type ";"`.
fn type_alias_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `type`
    name(p);
    generic_params(p);
    // A missing `=` still parses the value that follows, so recovery reports
    // only the absent token.
    if !p.eat(SyntaxKind::Eq) {
        p.error(ParseErrorKind::MissingToken);
    }
    if !p.at(SyntaxKind::Semi) {
        super::types::type_(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::TypeAliasDecl);
}

/// `"const" IDENT ":" Type "=" ConstExpression ";"`.
fn const_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `const`
    name(p);
    p.expect(SyntaxKind::Colon);
    super::types::type_(p);
    // A missing `=` still parses the value that follows, so recovery reports
    // only the absent token; an `=` with no value is a missing expression.
    let eq = p.eat(SyntaxKind::Eq);
    if !eq {
        p.error(ParseErrorKind::MissingToken);
    }
    if eq || !p.at(SyntaxKind::Semi) {
        super::expr::expr(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ConstDecl);
}

/// `("fn" | "action" | "task") IDENT GenericParams? "(" ParameterList ")"
/// ReturnType WhereClause? CapabilityClause? Block`, completed as `kind`. The three
/// callable forms share one shape.
fn fn_like_decl(p: &mut Parser, kind: SyntaxKind) {
    let m = p.start();
    p.bump_any(); // `fn` / `action` / `task`
    name(p);
    generic_params(p);
    param_list(p);
    return_type(p);
    where_clause(p);
    capability_clause(p);
    super::stmt::block(p);
    m.complete(p, kind);
}

/// `"(" (Parameter ("," Parameter)* ","?)? ")"` — a callable's parameter list.
fn param_list(p: &mut Parser) {
    let m = p.start();
    p.expect(SyntaxKind::LParen);
    while !p.at(SyntaxKind::RParen) && !p.at_end() {
        param(p);
        if !p.eat(SyntaxKind::Comma) {
            break;
        }
    }
    p.expect(SyntaxKind::RParen);
    m.complete(p, SyntaxKind::ParamList);
}

/// `"mut"? IDENT ":" Type ("=" DefaultExpression)?` — one parameter.
fn param(p: &mut Parser) {
    let m = p.start();
    p.eat(SyntaxKind::MutKw);
    name(p);
    p.expect(SyntaxKind::Colon);
    super::types::type_(p);
    if p.eat(SyntaxKind::Eq) {
        super::expr::expr(p);
    }
    m.complete(p, SyntaxKind::Param);
}

/// `("->" Type)?` — an optional return type, wrapped in a `ReturnType` node when
/// present.
fn return_type(p: &mut Parser) {
    if p.at(SyntaxKind::Arrow) {
        let m = p.start();
        p.bump_any(); // `->`
        super::types::type_(p);
        m.complete(p, SyntaxKind::ReturnType);
    }
}

/// `("requires" "{" CapabilityPath ("," CapabilityPath)* ","? "}")?` — an optional
/// capability clause. Each capability path is a plain type path.
fn capability_clause(p: &mut Parser) {
    if p.at_contextual(SyntaxKind::RequiresKw) && p.nth(1) == SyntaxKind::LBrace {
        let m = p.start();
        p.bump_as(SyntaxKind::RequiresKw);
        p.expect(SyntaxKind::LBrace);
        while !p.at(SyntaxKind::RBrace) && !p.at_end() {
            super::types::type_(p);
            if !p.eat(SyntaxKind::Comma) {
                break;
            }
        }
        p.expect(SyntaxKind::RBrace);
        m.complete(p, SyntaxKind::CapabilityClause);
    }
}

/// `("<" GenericParam ("," GenericParam)* ","? ">")?` — an optional generic
/// parameter list. A parameter is `IDENT (":" TraitBounds)? ("=" Type)?` or
/// `"const" IDENT ":" Type ("=" ConstExpression)?`.
fn generic_params(p: &mut Parser) {
    if p.at(SyntaxKind::Lt) {
        let m = p.start();
        p.bump_any(); // `<`
        while !p.at_gt() && !p.at_end() {
            let g = p.start();
            if p.eat(SyntaxKind::ConstKw) {
                name(p);
                p.expect(SyntaxKind::Colon);
                super::types::type_(p);
                if p.eat(SyntaxKind::Eq) {
                    super::expr::const_arg_expr(p);
                }
            } else {
                name(p);
                if p.eat(SyntaxKind::Colon) {
                    super::types::trait_bounds(p);
                }
                if p.eat(SyntaxKind::Eq) {
                    super::types::type_(p);
                }
            }
            g.complete(p, SyntaxKind::GenericParam);
            if !p.eat(SyntaxKind::Comma) {
                break;
            }
        }
        p.expect_gt();
        m.complete(p, SyntaxKind::GenericParams);
    }
}

/// `("implements" TraitBound ("+" TraitBound)*)?` — an optional implements clause.
fn implements_clause(p: &mut Parser) {
    if p.at(SyntaxKind::ImplementsKw) {
        let m = p.start();
        p.bump_any(); // `implements`
        super::types::trait_bounds(p);
        m.complete(p, SyntaxKind::ImplementsClause);
    }
}

/// `("where" WherePredicate ("," WherePredicate)* ","?)?` — an optional where
/// clause. Each predicate is `Type ":" TraitBounds`.
fn where_clause(p: &mut Parser) {
    if p.at(SyntaxKind::WhereKw) {
        let m = p.start();
        p.bump_any(); // `where`
        while super::types::at_type_start(p) {
            super::types::type_(p);
            p.expect(SyntaxKind::Colon);
            super::types::trait_bounds(p);
            if !p.eat(SyntaxKind::Comma) {
                break;
            }
        }
        m.complete(p, SyntaxKind::WhereClause);
    }
}

/// A Standard/Advanced declaration whose full grammar has no dedicated node kind
/// this slice. It is consumed up to and including its brace group (or terminating
/// `;`) as a balanced run and wrapped in an [`SyntaxKind::AdvancedItem`], so it
/// parses losslessly without contributing to resolution.
fn advanced_decl(p: &mut Parser, contextual: Option<SyntaxKind>) {
    let m = p.start();
    match contextual {
        Some(kw) => p.bump_as(kw),
        None => p.bump_any(), // the leading strict keyword
    }
    loop {
        match p.current() {
            SyntaxKind::LBrace => {
                skip_braced_group(p);
                break;
            }
            SyntaxKind::Semi => {
                p.bump_any();
                break;
            }
            _ if p.at_end() => break,
            _ => p.bump_any(),
        }
    }
    m.complete(p, SyntaxKind::AdvancedItem);
}

/// Consumes a balanced `{ ... }` group, tracking brace depth so nested groups are
/// skipped whole. Used to swallow the body of an advanced declaration.
fn skip_braced_group(p: &mut Parser) {
    let mut depth = 0usize;
    loop {
        match p.current() {
            SyntaxKind::LBrace => {
                depth += 1;
                p.bump_any();
            }
            SyntaxKind::RBrace => {
                depth -= 1;
                p.bump_any();
                if depth == 0 {
                    break;
                }
            }
            _ if p.at_end() => break,
            _ => p.bump_any(),
        }
    }
}
