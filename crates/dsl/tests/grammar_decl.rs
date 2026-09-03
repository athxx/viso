//! Commit-2 grammar tests: the declaration surface of a `.vs` compilation unit —
//! imports/exports, component/system members, record/enum/type/const, and the
//! callable forms. These assert the *shape* of the typed tree the declaration
//! grammar produces, the `:` (type/binding) vs `=` (initializer) split, multi-error
//! recovery, and that every input round-trips byte-for-byte.

use viso_dsl::syntax::grammar::{Entry, parse, parse_entry};
use viso_dsl::syntax::{ParseErrorKind, SyntaxKind, SyntaxNode, tokenize};

/// Parses `src` as a `.vs` compilation unit and returns the red root.
fn unit(src: &str) -> SyntaxNode {
    SyntaxNode::new_root(parse(&tokenize(src), src).root)
}

/// Parses `src` as a `component!` entry and returns the red root.
fn component_entry(src: &str) -> SyntaxNode {
    SyntaxNode::new_root(parse_entry(&tokenize(src), src, Entry::ComponentEntry).root)
}

/// Whether `src`, parsed as a compilation unit, produces at least one error of
/// `kind`.
fn unit_has_error(src: &str, kind: ParseErrorKind) -> bool {
    parse(&tokenize(src), src)
        .errors
        .iter()
        .any(|e| e.kind == kind)
}

/// The first descendant node of `root` whose kind is `kind`.
fn first(root: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    root.descendants().into_iter().find(|n| n.kind() == kind)
}

/// The number of descendant nodes of `root` whose kind is `kind`.
fn count(root: &SyntaxNode, kind: SyntaxKind) -> usize {
    root.descendants()
        .into_iter()
        .filter(|n| n.kind() == kind)
        .count()
}

#[test]
fn compilation_unit_root_and_lossless() {
    let src = "import a::b;\ncomponent C { }\n";
    let root = unit(src);
    assert_eq!(root.kind(), SyntaxKind::CompilationUnit);
    assert_eq!(root.text(), src, "the compilation unit round-trips");
}

#[test]
fn import_with_rename_and_selective_items() {
    let renamed = unit("import a::b as c;");
    assert!(first(&renamed, SyntaxKind::ImportDecl).is_some());
    assert!(
        first(&renamed, SyntaxKind::RenameClause).is_some(),
        "`as c` is a rename clause"
    );

    let selective = unit("import a::b::{x, y as z};");
    assert_eq!(
        count(&selective, SyntaxKind::ImportItem),
        2,
        "two selective import items"
    );
}

#[test]
fn export_wraps_the_declaration() {
    let root = unit("export component C { }");
    let export = first(&root, SyntaxKind::ExportDecl).expect("an ExportDecl");
    assert!(
        first(&export, SyntaxKind::ComponentDecl).is_some(),
        "the export wraps the component declaration"
    );
}

#[test]
fn input_uses_colon_for_type_and_equals_for_default() {
    // `input x: T = e;` — the `:` introduces the type, the `=` the default.
    let root = unit("component C { input label: Text = \"hi\"; }");
    let input = first(&root, SyntaxKind::InputDecl).expect("an InputDecl");
    assert!(
        first(&input, SyntaxKind::TypePath).is_some(),
        "the input carries a typed annotation after `:`"
    );
    // The default initializer after `=` is an expression, not a type.
    assert!(
        first(&input, SyntaxKind::LiteralExpr).is_some(),
        "the `=` default is an expression"
    );
}

#[test]
fn state_infers_type_and_requires_an_initializer() {
    // `state x = e;` — no `:` type, only the `=` initializer.
    let root = unit("component C { state n = 0; }");
    let state = first(&root, SyntaxKind::StateDecl).expect("a StateDecl");
    assert!(
        first(&state, SyntaxKind::TypePath).is_none(),
        "an inferred state has no type annotation"
    );
    assert!(first(&state, SyntaxKind::LiteralExpr).is_some());

    // A state with no initializer is a missing-token error.
    assert!(unit_has_error(
        "component C { state n; }",
        ParseErrorKind::MissingToken
    ));
}

#[test]
fn record_fields_and_enum_variants_parse() {
    let rec = unit("record Point { x: F32; y: F32 = 0.0; }");
    assert_eq!(count(&rec, SyntaxKind::RecordField), 2);

    let en = unit("enum Shape { Circle(F32); Rect { w: F32; h: F32; }; Empty; }");
    assert_eq!(count(&en, SyntaxKind::EnumVariant), 3);
    assert_eq!(
        count(&en, SyntaxKind::VariantPayload),
        2,
        "the tuple and record variants carry payloads; `Empty` does not"
    );
}

#[test]
fn callable_forms_share_one_shape() {
    let root = unit("fn f(mut x: I32, y: I32 = 1) -> I32 { x }");
    let f = first(&root, SyntaxKind::FnDecl).expect("an FnDecl");
    assert!(first(&f, SyntaxKind::ParamList).is_some());
    assert_eq!(count(&f, SyntaxKind::Param), 2, "two parameters");
    assert!(
        first(&f, SyntaxKind::ReturnType).is_some(),
        "the `-> I32` return type"
    );

    // `action` and `task` reuse the same shape under their own kinds.
    assert!(first(&unit("action a() { }"), SyntaxKind::ActionDecl).is_some());
    assert!(first(&unit("task t() { }"), SyntaxKind::TaskDecl).is_some());
}

#[test]
fn advanced_declarations_are_parsed_but_not_gated() {
    // A `trait` has no dedicated node kind this slice; it is swallowed whole as an
    // AdvancedItem, so it neither breaks the tree nor gates the slice.
    let root = unit("trait Drawable { fn draw(); }\ncomponent C { }");
    assert!(first(&root, SyntaxKind::AdvancedItem).is_some());
    // The component after it still parses to its real kind.
    assert!(first(&root, SyntaxKind::ComponentDecl).is_some());
    assert_eq!(
        root.text(),
        "trait Drawable { fn draw(); }\ncomponent C { }"
    );
}

#[test]
fn component_entry_routes_through_one_grammar() {
    // `component!` parses the same component grammar as a `.vs` file's component.
    let root = component_entry("component C { state n = 0; }");
    assert_eq!(root.kind(), SyntaxKind::ComponentEntry);
    assert!(first(&root, SyntaxKind::ComponentDecl).is_some());
    assert!(first(&root, SyntaxKind::StateDecl).is_some());
}

#[test]
fn multi_error_recovery_keeps_going() {
    // Two malformed members in a row: recovery must record more than one error and
    // still parse the well-formed member that follows.
    let src = "component C { % ! state n = 0; }";
    let parse = parse(&tokenize(src), src);
    assert!(
        parse.errors.len() >= 2,
        "recovery records multiple errors, got {:?}",
        parse.errors
    );
    let root = SyntaxNode::new_root(parse.root);
    assert!(
        first(&root, SyntaxKind::StateDecl).is_some(),
        "the good member after the garbage still parses"
    );
    assert_eq!(root.text(), src, "recovery stays lossless");
}

#[test]
fn declaration_losslessness_holds_over_a_corpus() {
    let corpus = [
        "import a;",
        "import a::b::c as d;",
        "import a::{b, c as d,};",
        "export record R { x: I32; }",
        "component C<T: Bounded> implements Trait where T: Other { input x: T; }",
        "system S { computed total: I32 = 1 + 2; }",
        "enum E { A; B(I32); C { f: F32; }; }",
        "type Alias = Map<K, V>;",
        "const MAX: I32 = 100;",
        "fn f() { }",
        "action a(x: I32) { }",
        "impl Foo for Bar { }",
        "template T { }",
    ];
    for src in corpus {
        let root = unit(src);
        assert_eq!(root.kind(), SyntaxKind::CompilationUnit);
        assert_eq!(root.text(), src, "round-trip {src:?}");
    }
}
