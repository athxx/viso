//! The parser golden corpus: for every Core production, one positive case that
//! parses with no diagnostics and one negative case with the stable codes it must
//! produce. `parser_golden.rs` checks the parser against it; the formatter's golden
//! test in `viso-lsp` checks formatting over it.

use viso_dsl::syntax::grammar::Entry;

/// One golden case: the entry production, the source, and the sorted, deduped
/// set of diagnostic codes it must produce (empty for a positive case).
pub struct Case {
    pub production: &'static str,
    pub entry: Entry,
    pub src: &'static str,
    pub codes: &'static [&'static str],
}

const fn unit(production: &'static str, src: &'static str, codes: &'static [&'static str]) -> Case {
    Case {
        production,
        entry: Entry::CompilationUnit,
        src,
        codes,
    }
}

const fn expr(production: &'static str, src: &'static str, codes: &'static [&'static str]) -> Case {
    Case {
        production,
        entry: Entry::Expr,
        src,
        codes,
    }
}

const fn frag(production: &'static str, src: &'static str, codes: &'static [&'static str]) -> Case {
    Case {
        production,
        entry: Entry::ViewFragment,
        src,
        codes,
    }
}

const OK: &[&str] = &[];

pub const CASES: &[Case] = &[
    // --- Compilation unit / imports -------------------------------------
    unit("ImportDecl", "import a::b;", OK),
    unit("ImportDecl", "import a::;", &["E1404"]),
    unit("ImportDecl alias", "import a::b as c;", OK),
    unit("ImportDecl alias", "import a::b as fn;", &["E1301"]),
    // --- Component and members -------------------------------------------
    unit("ComponentDecl", "component C { }", OK),
    unit("ComponentDecl", "component C {", &["E1401"]),
    unit(
        "InputDecl",
        "component C { input title: String = \"t\"; }",
        OK,
    ),
    unit(
        "InputDecl",
        "component C { input title String; }",
        &["E1404"],
    ),
    unit("StateDecl", "component C { state count = 0; }", OK),
    unit("StateDecl", "component C { state fn = 0; }", &["E1301"]),
    unit("ComputedDecl", "component C { computed d: I64 = 1; }", OK),
    unit(
        "ComputedDecl",
        "component C { computed d: I64; }",
        &["E1404"],
    ),
    unit(
        "EventDecl",
        "component C { event tapped(x: I64, y: I64); }",
        OK,
    ),
    unit("EventDecl", "component C { event tapped; }", &["E1404"]),
    unit("SlotDecl", "component C { slot header: View = empty; }", OK),
    unit("SlotDecl", "component C { slot header View; }", &["E1404"]),
    unit(
        "ActionDecl",
        "component C { action inc() { count += 1; } }",
        OK,
    ),
    unit("ActionDecl", "component C { action inc( { } }", &["E1404"]),
    unit("ViewDecl", "component C { view { Text { } } }", OK),
    unit("ViewDecl", "component C { view { Text { } }", &["E1401"]),
    // --- Contextual keywords as ordinary names (§12.3) --------------------
    unit(
        "contextual as name",
        "component C { state state = 0; state view = 1; state key = 2; }",
        OK,
    ),
    unit(
        "contextual as name",
        "fn f() { let move = 1; let emit = 2; let style = move + emit; }",
        OK,
    ),
    unit("strict as name", "fn f() { let match = 1; }", &["E1301"]),
    // --- Top-level declarations -----------------------------------------
    unit("FnDecl", "fn add(a: I64, b: I64) -> I64 { a + b }", OK),
    unit(
        "FnDecl",
        "fn add(a: I64, b: I64 -> I64 { a + b }",
        &["E1404"],
    ),
    unit("RecordDecl", "record P { x: F32; y: F32 = 0.0; }", OK),
    unit("RecordDecl", "record P { x F32; }", &["E1404"]),
    unit(
        "RecordDecl recovery",
        "record P { x: F32, y: F32 }",
        &["E1403", "E1404"],
    ),
    unit("RecordDecl label", "record P { type: I64; fn: I64; }", OK),
    unit(
        "EnumDecl",
        "enum S { idle; busy(I64); done { code: I64; }; type; }",
        OK,
    ),
    unit("EnumDecl", "enum S { idle busy; }", &["E1404"]),
    unit("TypeAlias", "type Id = I64;", OK),
    unit("TypeAlias", "type Id I64;", &["E1404"]),
    unit("ConstDecl", "const N: I64 = 4;", OK),
    unit("ConstDecl", "const N: I64 4;", &["E1404"]),
    // --- Generics (§26) --------------------------------------------------
    unit(
        "GenericParams",
        "fn id<T: Clone, const N: I64 = 4>(x: T) -> T { x }",
        OK,
    ),
    unit("GenericParams", "fn id<T(x: T) -> T { x }", &["E1404"]),
    unit("GenericArgs", "type M = Matrix<F32, const 4>;", OK),
    unit("GenericArgs", "type M = Matrix<F32, 4>;", &["E2004"]),
    unit("GenericArgs nested", "type M = List<List<I64>>;", OK),
    // --- Statements -----------------------------------------------------
    unit("LetStmt", "fn f() { let x: I64 = 1; }", OK),
    unit("LetStmt", "fn f() { let = 1; }", &["E1404"]),
    unit("LetStmt pattern", "fn f() { let (a, mut b) = p; }", OK),
    unit("AssignStmt", "fn f() { x = 1; x += 2; }", OK),
    unit("AssignStmt", "fn f() { x = ; }", &["E1405"]),
    unit("ReturnStmt", "fn f() -> I64 { return 1; }", OK),
    unit(
        "IfStmt",
        "fn f() { if a { b(); } else if c { d(); } else { e(); } }",
        OK,
    ),
    unit("IfStmt", "fn f() { if Foo { x: 1 } { } }", &["E2801"]),
    unit("WhileStmt", "fn f() { while a < b { a += 1; } }", OK),
    unit("ForStmt", "fn f() { for (i, x) in xs { g(i, x); } }", OK),
    unit("ForStmt", "fn f() { for x xs { } }", &["E1404"]),
    unit("MatchStmt", "fn f() { match x { 1 => a(), _ => b() } }", OK),
    unit("BlockStmt", "fn f() { { let x = 1; } }", OK),
    unit("TailExpr", "fn f() -> I64 { let x = 1; x + 1 }", OK),
    unit(
        "EmitStmt",
        "component C { event done(); action go() { emit done(); } }",
        OK,
    ),
    unit(
        "EmitStmt",
        "component C { action go() { emit done(x; } }",
        &["E1404"],
    ),
    unit("emit as callee", "fn f() { emit(1); }", OK),
    unit(
        "TransactionStmt",
        "component C { action go() { transaction { a = 1; b = 2; } } }",
        OK,
    ),
    unit("transaction as name", "fn f() { let transaction = 1; }", OK),
    // --- Expressions ----------------------------------------------------
    expr("BinaryExpr", "a + b * c - d / e", OK),
    expr("BinaryExpr", "a + ", &["E1405"]),
    expr("ComparisonExpr", "a < b", OK),
    expr("ComparisonExpr", "a < b < c", &["E2802"]),
    expr("EqualityExpr", "a == b", OK),
    expr("EqualityExpr", "a == b != c", &["E2802"]),
    expr("RangeExpr", "a..=b", OK),
    expr("RangeExpr", "a .. b .. c", &["E2802"]),
    expr("UnaryExpr", "-a * !b + ~c", OK),
    expr("CallExpr", "f(1, name: 2)", OK),
    expr("CallExpr", "f(1, 2", &["E1401"]),
    expr("CallExpr", "f(1, )", OK),
    expr("CallExpr", "f(1 2)", &["E1404"]),
    expr("Turbofish", "make::<Foo, const 4>(x)", OK),
    expr("Turbofish", "make<Foo>(x)", &["E2004"]),
    expr("Turbofish const", "make::<Foo, 4>(x)", &["E2004"]),
    expr("FieldExpr", "a.b.type", OK),
    expr("OptionalFieldExpr", "a?.b", OK),
    expr("IndexExpr", "a[1]", OK),
    expr("IndexExpr", "a[]", &["E1405"]),
    expr("TryExpr", "f()?", OK),
    expr("CastExpr", "x as I64", OK),
    expr("RecordExpr", "P { x: 1, y, ..base }", OK),
    expr("RecordExpr", "P { x: , y }", &["E1405"]),
    expr("ListExpr", "[1, 2, 3]", OK),
    expr("ListExpr", "[1, 2", &["E1401"]),
    expr("TupleExpr", "(a, b)", OK),
    expr("ParenExpr", "(a + b)", OK),
    expr("ClosureExpr", "|x, (a, b): P| x + a", OK),
    expr("ClosureExpr", "move || { a; b }", OK),
    expr("IfExpr", "if c { a } else { b }", OK),
    expr(
        "MatchExpr",
        "match v { S::idle => 0, S::busy(n) if n > 0 => n, _ => 1 }",
        OK,
    ),
    expr("MatchExpr", "match v { S::idle 0 }", &["E1404"]),
    // --- Patterns (A.13) ------------------------------------------------
    expr("WildcardPattern", "match v { _ => 0 }", OK),
    expr(
        "LiteralPattern",
        "match v { 1 => a, -1 => b, 'c' => c, \"s\" => d, true => e, None => f }",
        OK,
    ),
    expr("IdentPattern", "match v { mut x => x }", OK),
    expr("IdentPattern", "match v { fn => 0 }", &["E1301"]),
    expr("OrPattern", "match v { 1 | 2 | 3 => a }", OK),
    expr("BindingPattern", "match v { n @ 1..=9 => n }", OK),
    expr("RangePattern", "match v { 0..10 => a, 'a'..='z' => b }", OK),
    expr("TuplePattern", "match v { (a, _, (b, c)) => a }", OK),
    expr("TuplePattern", "match v { (a, => a }", &["E1404"]),
    expr(
        "ListPattern",
        "match v { [first, .., last] => first, [] => 0, [x, ..rest] => x }",
        OK,
    ),
    expr(
        "ConstructorPattern",
        "match v { S::busy(n) => n, P { x, y: 0, .. } => x }",
        OK,
    ),
    expr(
        "ConstructorPattern",
        "match v { S::busy(n => n }",
        &["E1404"],
    ),
    expr(
        "QualifiedVariantPattern",
        "match v { S::idle => 0, a::S::done => 1 }",
        OK,
    ),
    expr(
        "QualifiedVariantPattern label",
        "match v { S::type => 0 }",
        OK,
    ),
    expr("ParenPattern", "match v { (a | b) => 0 }", OK),
    // --- View (§14) -----------------------------------------------------
    frag("AnonymousNode", "Text { text: \"hi\"; }", OK),
    frag("AnonymousNode", "Text { text: \"hi\" }", &["E1404"]),
    frag("NamedNode", "node title: Text { text: label; }", OK),
    frag("NamedNode", "node title: { }", &["E1404"]),
    frag(
        "PropertyBinding keyword path",
        "Text { style.type: 1; }",
        OK,
    ),
    frag("ChildReserved", "child Text { }", &["E3001"]),
    frag("EventHandler", "Button { on click { count += 1; } }", OK),
    frag(
        "EventHandler",
        "Button { on click => count += 1; }",
        &["E3201"],
    ),
    frag(
        "EventHandler payload",
        "Button { on changed(v) { value = v; } }",
        OK,
    ),
    frag("BindClause", "Input { bind value <=> name; }", OK),
    frag("BindClause", "Input { bind value <=> ; }", &["E1404"]),
    frag("ViewIf", "if show { Text { } } else { Spacer { } }", OK),
    frag(
        "ViewFor",
        "for item in items key item.id { Text { text: item.name; } }",
        OK,
    ),
    frag("ViewFor", "for item in items { Text { } }", &["E3401"]),
    frag(
        "ViewMatch",
        "match mode { M::a => { Text { } }, _ => { Spacer { } } }",
        OK,
    ),
];
