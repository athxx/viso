//! The declaration grammar (Appendix A.2, A.4–A.7). A compilation unit is a run
//! of imports followed by top-level declarations; a component/system body is a run
//! of typed members. This is the declarative surface of the `:` vs `=` split — a
//! `:` separates a name from its type (`input x: T`, `field: T`, `name: Type`),
//! while `=` gives an initializer or default (`state x = e`, `const C: T = e`).
//!
//! ## Core versus Advanced
//!
//! Only the Core and a few Standard forms get dedicated node kinds and later
//! resolution. Advanced declarations (`trait`/`impl`/`template`/`style`/`theme`/
//! `shader`/`native`, and the Standard `effect`/`resource`) are parsed just enough
//! to consume their body — their brace group is skipped as a balanced run — and
//! wrapped in a single [`SyntaxKind::AdvancedItem`] so they neither break the tree
//! nor gate the slice. Their resolution lands when their consumer does.

use super::super::kind::SyntaxKind;
use super::{ParseErrorKind, Parser};

/// `ImportDecl* TopLevelDecl* EOF` — the body of a `.vs` file / `view!` entry.
pub(super) fn compilation_unit(p: &mut Parser) {
    while !p.at_end() {
        if p.at(SyntaxKind::ImportKw) {
            import_decl(p);
        } else {
            top_level_decl(p);
        }
    }
}

/// `ImportDecl* ComponentDecl EOF` — the body of a `component!` entry. Imports may
/// precede the single component; anything else is still parsed (and recovered) so
/// a malformed entry does not swallow the rest of the input.
pub(super) fn component_entry(p: &mut Parser) {
    while !p.at_end() {
        if p.at(SyntaxKind::ImportKw) {
            import_decl(p);
        } else {
            top_level_decl(p);
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

/// `IDENT ("::" IDENT)*` — a module path.
fn module_path(p: &mut Parser) {
    let m = p.start();
    name(p);
    while p.at(SyntaxKind::ColonColon) && p.nth(1) != SyntaxKind::LBrace {
        p.bump_any(); // `::`
        name(p);
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
fn top_level_decl(p: &mut Parser) {
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
        // Standard/Advanced declarations parsed but not resolved this slice.
        SyntaxKind::TraitKw
        | SyntaxKind::ImplKw
        | SyntaxKind::TemplateKw
        | SyntaxKind::StyleKw
        | SyntaxKind::ThemeKw
        | SyntaxKind::ShaderKw
        | SyntaxKind::NativeKw
        | SyntaxKind::EffectKw
        | SyntaxKind::ResourceKw => advanced_decl(p),
        _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
    }
}

/// `"component" IDENT GenericParams? ImplementsClause? WhereClause? "{"
/// ComponentMember* "}"`.
fn component_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `component`
    name(p);
    generic_params(p);
    implements_clause(p);
    where_clause(p);
    member_block(p, member);
    m.complete(p, SyntaxKind::ComponentDecl);
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
        f(p);
    }
    p.expect(SyntaxKind::RBrace);
}

/// One component/system member: dispatched on its leading keyword.
fn member(p: &mut Parser) {
    attributes(p);
    match p.current() {
        SyntaxKind::InputKw => input_decl(p),
        SyntaxKind::StateKw => state_decl(p),
        SyntaxKind::ComputedKw => computed_decl(p),
        SyntaxKind::EventKw => event_decl(p),
        SyntaxKind::SlotKw => slot_decl(p),
        SyntaxKind::ConstKw => const_decl(p),
        SyntaxKind::FnKw => fn_like_decl(p, SyntaxKind::FnDecl),
        SyntaxKind::ActionKw => fn_like_decl(p, SyntaxKind::ActionDecl),
        SyntaxKind::TaskKw => fn_like_decl(p, SyntaxKind::TaskDecl),
        SyntaxKind::ViewKw => super::view::view_decl(p),
        SyntaxKind::EffectKw | SyntaxKind::ResourceKw | SyntaxKind::NativeKw => advanced_decl(p),
        _ => p.err_and_bump(ParseErrorKind::UnexpectedTokens),
    }
}

/// `"input" IDENT ":" Type ("=" DefaultExpression)? ";"`.
fn input_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `input`
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
    p.bump_any(); // `state`
    name(p);
    if p.eat(SyntaxKind::Colon) {
        super::types::type_(p);
    }
    if p.eat(SyntaxKind::Eq) {
        super::expr::expr(p);
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::StateDecl);
}

/// `"computed" IDENT (":" Type)? "=" Expression ";"`.
fn computed_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `computed`
    name(p);
    if p.eat(SyntaxKind::Colon) {
        super::types::type_(p);
    }
    if p.eat(SyntaxKind::Eq) {
        super::expr::expr(p);
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ComputedDecl);
}

/// `"event" IDENT "(" EventParameterList ")" ";"`.
fn event_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // `event`
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
    p.bump_any(); // `slot`
    name(p);
    p.expect(SyntaxKind::Colon);
    super::types::type_(p);
    if p.eat(SyntaxKind::Eq) {
        // The default is `None` or the `empty` context word; consume either.
        if p.at(SyntaxKind::NoneKw) || p.at(SyntaxKind::Ident) {
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
        record_field(p);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::RecordDecl);
}

/// `Attribute* IDENT ":" Type ("=" ConstExpression)? ";"` — one record field.
fn record_field(p: &mut Parser) {
    let m = p.start();
    attributes(p);
    name(p);
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
        enum_variant(p);
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::EnumDecl);
}

/// `Attribute* IDENT VariantPayload? ";"` where a payload is a tuple `"(" TypeList
/// ")"` or a record `"{" RecordField* "}"`.
fn enum_variant(p: &mut Parser) {
    let m = p.start();
    attributes(p);
    name(p);
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
                record_field(p);
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
    if p.eat(SyntaxKind::Eq) {
        super::types::type_(p);
    } else {
        p.error(ParseErrorKind::MissingToken);
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
    if p.eat(SyntaxKind::Eq) {
        super::expr::expr(p);
    } else {
        p.error(ParseErrorKind::MissingToken);
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
    if p.at(SyntaxKind::RequiresKw) {
        let m = p.start();
        p.bump_any(); // `requires`
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
/// parameter list. Each parameter is a name with optional `: TraitBounds`.
fn generic_params(p: &mut Parser) {
    if p.at(SyntaxKind::Lt) {
        let m = p.start();
        p.bump_any(); // `<`
        while !p.at(SyntaxKind::Gt) && !p.at_end() {
            let g = p.start();
            name(p);
            if p.eat(SyntaxKind::Colon) {
                super::types::trait_bounds(p);
            }
            g.complete(p, SyntaxKind::GenericParam);
            if !p.eat(SyntaxKind::Comma) {
                break;
            }
        }
        p.expect(SyntaxKind::Gt);
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
fn advanced_decl(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // the leading keyword
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

/// Consumes any run of `@attr(...)` attributes preceding a declaration.
fn attributes(p: &mut Parser) {
    while p.at(SyntaxKind::At) {
        let m = p.start();
        p.bump_any(); // `@`
        super::expr::path_only(p);
        if p.at(SyntaxKind::LParen) {
            super::expr::arg_list(p);
        }
        m.complete(p, SyntaxKind::Attribute);
    }
}

/// Consumes an identifier name (plain or raw), recording a diagnostic if the
/// current token is not name-like.
fn name(p: &mut Parser) {
    if p.at(SyntaxKind::Ident) || p.at(SyntaxKind::RawIdent) {
        p.bump_any();
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
}
