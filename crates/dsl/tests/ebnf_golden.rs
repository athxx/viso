//! The normative EBNF (Appendix A) against the parser, one to one: every
//! production the appendix defines has exactly one row here, and no row names
//! a production the appendix lacks — the set is read from the spec itself, so
//! a production added to or dropped from the grammar fails this test until its
//! row follows. Each row's positive source parses through its entry with no
//! diagnostic; its negative source, broken where that production is, yields
//! exactly the pinned stable codes, lexer codes included. Both round-trip
//! byte for byte.
//!
//! Member sets a body shares with a component (a template's, a system's, a
//! `use` body's) and the shape of a style selector are restricted after
//! parsing, with the targeted codes the checker gives (`E3601`, `E2103`); their
//! negatives here break the production's syntax instead.

use std::collections::BTreeSet;

use viso_dsl::syntax::grammar::{Entry, parse_entry};
use viso_dsl::syntax::tokenize;

const SPEC: &str = include_str!("../../../Viso_DSL_1.0.md");

/// One production: its name, the entry both sources parse through, a source
/// using it, and a source broken in it with the codes that must follow.
struct Row {
    production: &'static str,
    entry: Entry,
    positive: &'static str,
    negative: &'static str,
    codes: &'static [&'static str],
}

use Entry::{
    BlockBody as B, CompilationUnit as U, ComponentEntry as K, ComponentMembers as C, Expr as E,
    NodeMembers as N, ViewFragment as F,
};

const fn row(
    production: &'static str,
    entry: Entry,
    positive: &'static str,
    negative: &'static str,
    codes: &'static [&'static str],
) -> Row {
    Row {
        production,
        entry,
        positive,
        negative,
        codes,
    }
}

#[rustfmt::skip]
const ROWS: &[Row] = &[
    // A.2 Compilation unit and declarations
    row("Label", E, "a.match", "a.", &["E1404"]),
    row("CompilationUnit", U, "import a::b;\nfn f() {}", "fn f() {} }", &["E1402"]),
    row("ViewFragment", F, "Text {} Spacer {}", "Text {} }", &["E1402"]),
    row("ComponentEntry", K, "import a::b; @x Card { view { Text {} } }", "component { }", &["E1404"]),
    row("ModulePath", U, "import a::b::type;", "import a::;", &["E1404"]),
    row("ImportDecl", U, "import a::b;", "import a::b", &["E1404"]),
    row("ImportSuffix", U, "import a::{B, C as D,};", "import a::{B C};", &["E1402", "E1403", "E1404"]),
    row("ImportItem", U, "import a::{B as C};", "import a::{B as};", &["E1404"]),
    row("TopLevelDecl", U, "@inline export fn f() {}", "export export fn f() {}", &["E1403"]),
    row("DeclCore", U, "export record R {}", "export 1;", &["E1403"]),
    row("Attribute", U, "@a::b(1, k: 2) fn f() {}", "@(1) fn f() {}", &["E1404"]),
    row("AttributeArgs", U, "@a(1, 2,) fn f() {}", "@a(1 2) fn f() {}", &["E1404"]),
    row("AttributeArg", U, "@a(k: 1) fn f() {}", "@a(k: ) fn f() {}", &["E1405"]),
    // A.3 Paths, generics and types
    row("Path", E, "a::b::type", "a::", &["E1404"]),
    row("TypePath", U, "type T = a::B<I64>::C<F32>;", "type T = a::;", &["E1404"]),
    row("GenericArgs", U, "type T = Map<I64, String,>;", "type T = Map<I64;", &["E1404"]),
    row("GenericArg", U, "type T = M<F32, const 4>;", "type T = M<const>;", &["E1404", "E1405"]),
    row("GenericParams", U, "fn f<T, const N: I64>() {}", "fn f<T() {}", &["E1404"]),
    row("GenericParam", U, "fn f<T = I64>() {}", "fn f<1>() {}", &["E1401", "E1404"]),
    row("TypeGenericParam", U, "fn f<T: A + B = C>() {}", "fn f<T: >() {}", &["E1404"]),
    row("ConstGenericParam", U, "fn f<const N: I64 = 4>() {}", "fn f<const N>() {}", &["E1404"]),
    row("TraitBounds", U, "fn f<T: A + B>() {}", "fn f<T: A + >() {}", &["E1404"]),
    row("ImplementsClause", U, "record R implements A + B {}", "record R implements {}", &["E1404"]),
    row("TraitBound", U, "record R implements a::B<I64> {}", "record R implements 1 {}", &["E1403", "E1404"]),
    row("WhereClause", U, "fn f<T>() where T: A, {}", "fn f<T>() where {}", &["E1404"]),
    row("WherePredicate", U, "fn f<T>() where List<T>: A + B {}", "fn f<T>() where T A {}", &["E1404"]),
    row("Type", U, "type T = Self;", "type T = ;", &["E1404"]),
    row("FunctionType", U, "type T = Fn(I64, F64) -> Bool;", "type T = Fn(I64);", &["E1404"]),
    row("TupleType", U, "type T = (I64, F64);", "type T = (I64, F64;", &["E1404"]),
    row("ArrayType", U, "type T = [I64; 4];", "type T = [I64; ];", &["E1405"]),
    row("SliceType", U, "type T = [I64];", "type T = [];", &["E1404"]),
    row("TraitObjectType", U, "type T = dyn A + B;", "type T = dyn ;", &["E1404"]),
    row("TypeList", U, "type T = Fn(I64, F64,) -> I64;", "type T = Fn(I64 F64) -> I64;", &["E1402", "E1403", "E1404"]),
    row("ConstExpression", U, "const N: I64 = 2 * 3;", "const N: I64 = 2 *;", &["E1405"]),
    row("DefaultExpression", U, "fn f(a: I64 = 1 + 2) {}", "fn f(a: I64 = ) {}", &["E1405"]),
    row("InitExpression", C, "state s = [1, 2];", "state s = [1, ;", &["E1404", "E1405"]),
    // A.4 Records, enums, traits and impls
    row("RecordDecl", U, "record R<T> implements A where T: B { x: T; }", "record { }", &["E1404"]),
    row("RecordField", U, "record R { @a x: I64 = 1; }", "record R { x: I64 = 1 }", &["E1404"]),
    row("EnumDecl", U, "enum E<T> { a; b(T); }", "enum { }", &["E1404"]),
    row("EnumVariant", U, "enum E { @a b(I64); }", "enum E { b(I64) }", &["E1404"]),
    row("VariantPayload", U, "enum E { a(I64, F64); b { x: I64; }; }", "enum E { a(I64; }", &["E1404"]),
    row("TraitDecl", U, "trait T<A>: B where A: C { fn f(self) -> I64; }", "trait { }", &["E1404"]),
    row("TraitMember", U, "trait T { fn f(); action a() {} task t(); type X; const N: I64; }", "trait T { state s = 1; }", &["E1403"]),
    row("AssociatedTypeDecl", U, "trait T { type X: A + B; }", "trait T { type X }", &["E1404"]),
    row("AssociatedConstDecl", U, "trait T { const N: I64; }", "trait T { const N; }", &["E1404"]),
    row("ImplDecl", U, "impl<T> A for B<T> where T: C { fn f() {} }", "impl { }", &["E1404"]),
    row("ImplTarget", U, "impl A for B {}\nimpl B {}", "impl A for {}", &["E1404"]),
    row("ImplMember", U, "impl B { fn f() {} action a() {} task t() {} type X = I64; const N: I64 = 1; }", "impl B { state s = 1; }", &["E1403"]),
    row("AssociatedTypeImpl", U, "impl B { type X = I64; }", "impl B { type X = ; }", &["E1404"]),
    row("AssociatedConstImpl", U, "impl B { const N: I64 = 1; }", "impl B { const N: I64 = ; }", &["E1405"]),
    row("TypeAliasDecl", U, "type M<T> = List<T>;", "type M = List<T>", &["E1404"]),
    row("ConstDecl", U, "const N: I64 = 4;", "const N = 4;", &["E1404"]),
    // A.5 Callables
    row("ParameterList", U, "fn f(a: I64, b: F64,) {}", "fn f(a: I64 b: F64) {}", &["E1401", "E1402", "E1403", "E1404"]),
    row("SelfParam", U, "impl B { fn f(mut self: B, a: I64) {} }", "impl B { fn f(mut self:) {} }", &["E1404"]),
    row("Parameter", U, "fn f(mut a: I64 = 1) {}", "fn f(a) {}", &["E1404"]),
    row("ReturnType", U, "fn f() -> I64 { 1 }", "fn f() -> { }", &["E1404"]),
    row("CapabilityClause", U, "fn f() requires { net::http, fs::read, } {}", "fn f() requires { } {}", &["E1404"]),
    row("CapabilityPath", U, "fn f() requires { a::b::c } {}", "fn f() requires { a:: } {}", &["E1404"]),
    row("FunctionDecl", U, "fn f<T>(x: T) -> T where T: A requires { a::b } { x }", "fn f() -> I64", &["E1401", "E1404"]),
    row("FunctionSignature", U, "trait T { fn f<A>(x: A) -> A where A: B requires { a::b }; }", "trait T { fn (x: A); }", &["E1404"]),
    row("ActionDecl", U, "action go(n: I64) requires { a::b } { }", "action go { }", &["E1404"]),
    row("ActionSignature", U, "trait T { action go(n: I64); }", "trait T { action go; }", &["E1404"]),
    row("TaskDecl", U, "task load(id: I64) -> Result<I64, String> { Ok(id) }", "task load { }", &["E1404"]),
    row("TaskSignature", U, "trait T { task load(id: I64) -> I64; }", "trait T { task load; }", &["E1404"]),
    // A.6 Components and systems
    row("ComponentDecl", U, "component C<T> implements A where T: B { }", "component { }", &["E1404"]),
    row("ComponentDeclBody", K, "Card { state s = 1; }", "Card state s = 1;", &["E1401", "E1404"]),
    row("ComponentMember", C, "@a state s = 1; input i: I64; computed c = s; event e(); slot x: View = empty; const N: I64 = 1; fn f() {} action a() {} task t() {} effect e2 { } resource r: R { load = t(); } native fn n(); view { }", "record R {}", &["E1402", "E1403"]),
    row("SystemDecl", U, "system S implements FixedUpdate { state x = 0; }", "system { }", &["E1404"]),
    row("SystemMember", U, "system S { @a input i: I64; state s = 1; computed c = s; const N: I64 = 1; fn f() {} action a() {} task t() {} effect e { } resource r: R { load = t(); } native fn n(); }", "system S { record R {} }", &["E1402", "E1403"]),
    row("InputDecl", C, "input i: I64 = 1;", "input i = 1;", &["E1404"]),
    row("StateDecl", C, "state s: I64 = 1;", "state s: I64;", &["E1404"]),
    row("ComputedDecl", C, "computed c: I64 = 1;", "computed c: I64;", &["E1404"]),
    row("EventDecl", C, "event e(a: I64);", "event e;", &["E1404"]),
    row("EventParameterList", C, "event e(a: I64, b: F64,);", "event e(a: I64 b: F64);", &["E1402", "E1403", "E1404"]),
    row("EventParameter", C, "event e(a: I64);", "event e(a);", &["E1404"]),
    row("SlotDecl", C, "slot s: View = None;", "slot s = empty;", &["E1404"]),
    row("SlotDefault", C, "slot s: View = empty;", "slot s: View = ;", &["E1404"]),
    row("EffectDecl", C, "effect e when (a, b) run Policy::latest { x(); cleanup { y(); } }", "effect e when a { }", &["E1401", "E1404"]),
    row("EffectDependencies", C, "effect e when (a, b,) { }", "effect e when () { }", &["E1405"]),
    row("EffectRunPolicy", C, "effect e run a::b { }", "effect e run { }", &["E1401", "E1404"]),
    row("CleanupClause", C, "effect e { cleanup { y(); } }", "effect e { cleanup y(); }", &["E1404"]),
    row("ResourceDecl", C, "resource r: Resource<I64, String> { load = f(); }", "resource r { load = f(); }", &["E1404"]),
    row("ResourceItem", C, "resource r: R { load = f(); key = k; policy = []; scope = s; }", "resource r: R { load f(); }", &["E1402", "E1403"]),
    row("PolicyList", C, "resource r: R { policy = [a, b,]; }", "resource r: R { policy = [a b]; }", &["E1402", "E1403", "E1404"]),
    row("StartStatement", B, "start load() as job { success(v) { } };", "start load() as ;", &["E1404"]),
    row("StartHandlerBlock", B, "start f() { policy = [a]; cancelled { } };", "start f() { x = 1; };", &["E1403"]),
    row("StartHandler", B, "start f() { success(v) { } error(e) { } cancelled { } };", "start f() { success { } };", &["E1402", "E1403", "E1404"]),
    // A.7 View
    row("ViewDecl", C, "view { Text {} }", "view Text {}", &["E1402", "E1403"]),
    row("ViewBlock", C, "view { }", "view { Text {}", &["E1401"]),
    row("ViewStructureItem", F, "@a Text {} node n: Text {} part p: Text {} if a { } for x in xs key x { } match m { _ => { } } use T(1);", "x = 1;", &["E1401", "E1403", "E1404"]),
    row("NamedNode", F, "node n: Text { }", "node n Text { }", &["E1401", "E1404"]),
    row("AnonymousNode", F, "Text { }", "Text", &["E1401", "E1404"]),
    row("PartNode", F, "part p: Text { }", "part p Text { }", &["E1401", "E1404"]),
    row("ComponentType", F, "a::Card<I64> { }", "a:: { }", &["E1404"]),
    row("NodeBody", F, "Text { text: a; }", "Text { text: a;", &["E1401"]),
    row("NodeMember", N, "@a text: a; bind v <=> s; on click { } fill f { } node n: T {} T {} part p: T {} if a { } for x in xs key x { } match m { _ => { } } use U(); override part p { } replace part q { }", "let x = 1;", &["E1403", "E1404", "E1405"]),
    row("PropertyBinding", N, "text: a;", "text = a;", &["E1404", "E1405"]),
    row("PropertyPath", N, "style.type.x: 1;", "style.: 1;", &["E1404"]),
    row("TwoWayBinding", N, "bind value <=> a.b[0] using Conv;", "bind value <=> ;", &["E1404"]),
    row("EventHandler", N, "on capture click(e) { }", "on click => go();", &["E3201"]),
    row("EventPhase", N, "on bubble click { }", "on capture bubble click { }", &["E1401", "E1404"]),
    row("FillClause", N, "fill header { Text {} }", "fill header Text {}", &["E1401", "E1404"]),
    row("ViewIf", F, "if a preserve \"x\" { } else if b { } else { }", "if a preserve x { }", &["E3301"]),
    row("ViewFor", F, "for (i, x) in xs key x.id { }", "for x in xs { }", &["E3401"]),
    row("ViewMatch", F, "match m { A::a => { }, _ => { }, }", "match m { _ => Text {} }", &["E1401", "E1404"]),
    row("ViewMatchArm", F, "match m { x if x > 0 => { } }", "match m { x => }", &["E1401", "E1404"]),
    row("PartOverride", N, "override part p { text: a; bind v <=> s; on click { } }", "override p { }", &["E1404"]),
    row("PartOverrideItem", N, "override part p { on click { } }", "override part p { Text {} }", &["E1404"]),
    row("PartReplace", N, "replace part p { Text {} }", "replace p { }", &["E1404"]),
    row("TemplateDecl", U, "template T<A>(x: I64) where A: B { slot s: View = empty; view { } }", "template T { view { } }", &["E1404"]),
    row("TemplateMember", U, "template T() { slot s: View = empty; const N: I64 = 1; fn f() {} view { } }", "template T() { slot s View; }", &["E1404"]),
    row("TemplateUse", F, "use a::T(1, k: 2) { fill f { } };", "use T(1) { }", &["E1404"]),
    row("TemplateUseBody", F, "use T() { fill f { } override part p { } replace part q { } };", "use T() { fill f };", &["E1401", "E1403", "E1404"]),
    // A.8 Style, theme, native and shader
    row("StyleDecl", U, "style S for Button: Base + Other { color: red; }", "style S Button { }", &["E1404"]),
    row("StyleBaseClause", U, "style S for B: a::X + Y { }", "style S for B: { }", &["E1404"]),
    row("StyleItem", U, "style S for B { color: red; when hover { color: blue; } }", "style S for B { x = 1; }", &["E1403"]),
    row("StyleWhen", U, "style S for B { when hover { color: blue; } }", "style S for B { when { } }", &["E1401", "E1404"]),
    row("StateSelector", U, "style S for B { when (hover || focus) && !disabled { } }", "style S for B { when (hover { } }", &["E1401", "E1404"]),
    row("SelectorOr", U, "style S for B { when a || b || c { } }", "style S for B { when a || { } }", &["E1405"]),
    row("SelectorAnd", U, "style S for B { when a && b { } }", "style S for B { when a && { } }", &["E1405"]),
    row("SelectorUnary", U, "style S for B { when !(a) { } }", "style S for B { when ! { } }", &["E1401", "E1404"]),
    row("ThemeDecl", U, "theme Dark: Base { accent = red; spacing = s; }", "theme { }", &["E1402", "E1403"]),
    row("ThemeItem", U, "theme D { accent = red; }", "theme D { accent: red; }", &["E1403"]),
    row("NativeDecl", U, "native fn f(x: I64) -> I64;", "native fn f(x: I64) -> I64 { x }", &["E1403"]),
    row("NativeMemberDecl", C, "native action go();", "native go();", &["E1402", "E1403", "E1404"]),
    row("NativeItem", U, "native task t();\nnative type H: A;", "native record R {}", &["E1403", "E1404"]),
    row("NativeFunction", U, "native fn f<T>(x: T) -> T where T: A requires { a::b };", "native fn f(x: I64)", &["E1404"]),
    row("NativeAction", U, "native action a(x: I64);", "native action a;", &["E1404"]),
    row("NativeTask", U, "native task t(x: I64) -> I64;", "native task t;", &["E1404"]),
    row("NativeTypeDecl", U, "native type H<T>: A + B where T: C;", "native type H = I64;", &["E1403", "E1404"]),
    row("ShaderDecl", U, "shader S { uniform u: F32; }", "shader { }", &["E1404"]),
    row("ShaderMember", U, "shader S { uniform u: F32; instance i: Vec4F32; varying v: Vec2F32; texture t: Texture2D; sampler s: Sampler; fn f(x: F32) -> F32 { x } vertex(p: Vec2F32) -> Vec4F32 { p } fragment(v: Vec2F32) -> Vec4F32 { v } compute(i: U32) -> Unit { } }", "shader S { state s = 1; }", &["E1403"]),
    row("ShaderUniform", U, "shader S { uniform u: F32; }", "shader S { uniform u F32; }", &["E1404"]),
    row("ShaderInstance", U, "shader S { instance i: Vec4F32; }", "shader S { instance i; }", &["E1404"]),
    row("ShaderVarying", U, "shader S { varying v: Vec2F32; }", "shader S { varying v: ; }", &["E1404"]),
    row("ShaderTexture", U, "shader S { texture t: Texture2D; }", "shader S { texture t: ; }", &["E1404"]),
    row("ShaderSampler", U, "shader S { sampler s: Sampler; }", "shader S { sampler s; }", &["E1404"]),
    row("ShaderFunction", U, "shader S { fn f(x: F32) -> F32 { x } }", "shader S { fn f(x: F32) -> { x } }", &["E1404"]),
    row("VertexEntry", U, "shader S { vertex(p: Vec2F32) -> Vec4F32 { p } }", "shader S { vertex(p: Vec2F32) -> Vec4F32 }", &["E1401", "E1404"]),
    row("FragmentEntry", U, "shader S { fragment(v: Vec2F32) -> Vec4F32 { v } }", "shader S { fragment v -> Vec4F32 { v } }", &["E1402", "E1403"]),
    row("ComputeEntry", U, "shader S { compute(i: U32) -> Unit { } }", "shader S { compute(i: U32) -> Unit; }", &["E1401", "E1403", "E1404"]),
    row("ShaderParameterList", U, "shader S { fn f(a: F32, b: F32,) -> F32 { a } }", "shader S { fn f(a: F32 b: F32) -> F32 { a } }", &["E1401", "E1402", "E1403", "E1404"]),
    row("ShaderParameter", U, "shader S { fn f(a: F32) -> F32 { a } }", "shader S { fn f(a) -> F32 { a } }", &["E1404"]),
    row("ShaderType", U, "shader S { uniform u: a::Vec4F32; }", "shader S { uniform u: ; }", &["E1404"]),
    // A.9 Statements
    row("Block", B, "{ let x = 1; x }", "{ let x = 1;", &["E1401"]),
    row("TailExpression", B, "let x = 1; x + 1", "x + ", &["E1405"]),
    row("Statement", B, "@a let x = 1;", "@(a) let x = 1;", &["E1404"]),
    row("StatementCore", B, "let a = 1; a = 2; f(); return; break; continue; emit e(); start f(); transaction { } if a { } match a { _ => 1 } while a { } for x in xs { } loop { }", "else { }", &["E1403"]),
    row("LetStatement", B, "let mut (a, b): (I64, I64) = p;", "let a = 1", &["E1404"]),
    row("AssignmentStatement", B, "a.b[0] += 1;", "a += ;", &["E1405"]),
    row("AssignmentOperator", B, "a -= 1; a *= 1; a /= 1; a %= 1; a &= 1; a |= 1; a ^= 1; a <<= 1; a >>= 1;", "a := 1;", &["E1403", "E1404"]),
    row("AssignablePath", B, "a.type[i].b = 1;", "a.[0] = 1;", &["E1404"]),
    row("AssignableSuffix", B, "a[i + 1] = 1;", "a[] = 1;", &["E1405"]),
    row("ExpressionStatement", B, "f(); g();", "f() g();", &["E1404"]),
    row("ReturnStatement", B, "return 1;", "return 1 2;", &["E1404"]),
    row("BreakStatement", B, "loop { break 1; }", "loop { break 1 2; }", &["E1404"]),
    row("ContinueStatement", B, "loop { continue; }", "loop { continue 1; }", &["E1404"]),
    row("EmitStatement", B, "emit done(1, k: 2);", "emit done;", &["E1404"]),
    row("TransactionStatement", B, "transaction { a = 1; }", "transaction a = 1;", &["E1404"]),
    row("IfStatement", B, "if a { } else if b { } else { }", "if a { } else b;", &["E1401", "E1404"]),
    row("MatchStatement", B, "match a { 1 => f(), _ => { } };", "match a { 1 => f() _ => g() }", &["E1404"]),
    row("WhileStatement", B, "while a < b { a += 1; }", "while { }", &["E1401", "E1404"]),
    row("ForStatement", B, "for (i, x) in xs { }", "for x in { }", &["E1401", "E1404"]),
    row("LoopStatement", B, "loop { break; }", "loop break;", &["E1401", "E1404"]),
    // A.10 Expressions
    row("Expression", E, "a ?? b .. c", "a +* b", &["E1405"]),
    row("HeadExpression", B, "while (P { x: 1 }).x > 0 { }", "while P { x: 1 } { }", &["E2801"]),
    row("RangeExpression", E, "a..b", "a .. b .. c", &["E2802"]),
    row("CoalesceExpression", E, "a ?? b ?? c", "a ?? ", &["E1405"]),
    row("LogicalOrExpression", E, "a || b || c", "a || ", &["E1405"]),
    row("LogicalAndExpression", E, "a && b && c", "a && ", &["E1405"]),
    row("ComparisonExpression", E, "a | b < c", "a < b > c", &["E2802"]),
    row("ComparisonOperator", E, "a <= b", "a =< b", &["E1403"]),
    row("BitOrExpression", E, "a | b | c", "a | ", &["E1405"]),
    row("BitXorExpression", E, "a ^ b ^ c", "a ^ ", &["E1405"]),
    row("BitAndExpression", E, "a & b & c", "a & ", &["E1405"]),
    row("ShiftExpression", E, "a << b >> c", "a << ", &["E1405"]),
    row("AdditiveExpression", E, "a + b - c", "a - ", &["E1405"]),
    row("MultiplicativeExpression", E, "a * b / c % d", "a * ", &["E1405"]),
    row("CastExpression", E, "x as I64 as F64", "x as ", &["E1404"]),
    row("UnaryExpression", E, "! ~ + - await a", "! ", &["E1405"]),
    row("PostfixExpression", E, "a.b::<I64>(c)[0]?.d?", "a.b(", &["E1401"]),
    row("PostfixSuffix", E, "a?.type", "a?.", &["E1404"]),
    row("GenericCallArgs", E, "f::<I64, const 2>()", "f::<I64()", &["E1404"]),
    row("PrimaryExpression", E, "(self, Self, [1], P { }, (a), { 1 }, if a { 1 } else { 2 }, match a { _ => 1 }, || 1)", ")", &["E1402"]),
    row("Literal", E, "[1, 1.5, \"s\", 'c', #fff, 8dp, true, false, None]", "1e2dp", &["E1203"]),
    row("TupleExpression", E, "(a, b, c,)", "(a, b", &["E1401"]),
    row("ListExpression", E, "[a, b,]", "[a b]", &["E1402", "E1403", "E1404"]),
    row("RecordExpression", E, "P::<I64> { x: 1 }", "P { x: 1", &["E1401"]),
    row("RecordInitializerList", E, "P { x: 1, y, ..b, }", "P { x: 1 y }", &["E1402", "E1403", "E1404"]),
    row("RecordInitializer", E, "P { ..base }", "P { x: }", &["E1405"]),
    row("IfExpression", E, "if a { 1 } else if b { 2 } else { 3 }", "if a { 1 } else", &["E1401", "E1404"]),
    row("MatchExpression", E, "match a { 1 => 2, _ => 3, }", "match a { 1 => 2", &["E1401", "E1404"]),
    row("MatchArm", E, "match a { x if x > 0 => { x } }", "match a { x 2 }", &["E1404"]),
    row("ClosureExpression", E, "move |x: I64| -> I64 { x }", "|x| -> ", &["E1404", "E1405"]),
    row("ClosureParams", E, "|a, mut b, (c, d): P,| a", "|a, b a", &["E1404"]),
    row("ClosureParameter", E, "|mut a: I64| a", "|a: | a", &["E1404"]),
    row("ExpressionList", C, "effect e when (a, b, c,) { }", "effect e when (a b) { }", &["E1401", "E1402", "E1404"]),
    row("ArgumentList", E, "f(a, k: b,)", "f(a b)", &["E1404"]),
    row("Argument", E, "f(type: 1)", "f(k: )", &["E1405"]),
    // A.13 Patterns
    row("Pattern", E, "match v { A::a(x) | A::b(x) => x }", "match v { | => 1 }", &["E1404", "E1405"]),
    row("OrPattern", E, "match v { 1 | 2 => a }", "match v { 1 | => a }", &["E1404"]),
    row("BindingPattern", E, "match v { mut n @ 1..=9 => n }", "match v { n @ => n }", &["E1404"]),
    row("RangePattern", E, "match v { 'a'..='z' => 1 }", "match v { 1..= => 1 }", &["E1404"]),
    row("PrimaryPattern", E, "match v { (_, 1, x) => x }", "match v { + => 1 }", &["E1403"]),
    row("LiteralPattern", E, "match v { -1 => a, \"s\" => b, 'c' => c, true => d, None => e }", "match v { -x => a }", &["E1403", "E1404"]),
    row("IdentifierPattern", E, "match v { mut x => x }", "match v { mut => 1 }", &["E1404"]),
    row("TuplePattern", E, "match v { (a, b,) => a }", "match v { (a, b => a }", &["E1404"]),
    row("ListPattern", E, "match v { [a, .., b] => a }", "match v { [a, b => a }", &["E1404"]),
    row("ListPatternItem", E, "match v { [x, ..rest] => x }", "match v { [x, ..1] => x }", &["E1402", "E1404", "E1405"]),
    row("ConstructorPattern", E, "match v { S::busy(n) => n }", "match v { S::busy(n => n }", &["E1404"]),
    row("ConstructorPatternPayload", E, "match v { P { x, y: 0, .. } => x }", "match v { P { x: } => x }", &["E1404"]),
    row("QualifiedVariantPattern", E, "match v { a::S::type => 0 }", "match v { S:: => 0 }", &["E1404", "E1405"]),
    row("RecordPatternField", E, "match v { P { type: t } => t }", "match v { P { 1 } => 0 }", &["E1402", "E1403", "E1404"]),
];

/// The productions Appendix A defines: every `Name` line followed by a
/// `::=` line inside its `ebnf` blocks.
fn appendix_productions() -> BTreeSet<&'static str> {
    let start = SPEC.find("# 附录 A").expect("Appendix A");
    let end = SPEC[start..]
        .find("# 附录 B")
        .map_or(SPEC.len(), |at| start + at);
    let mut names = BTreeSet::new();
    let mut in_ebnf = false;
    let mut previous = "";
    for line in SPEC[start..end].lines() {
        if line.starts_with("```") {
            in_ebnf = line == "```ebnf";
            continue;
        }
        if !in_ebnf {
            continue;
        }
        if line.trim_start().starts_with("::=") && !previous.is_empty() {
            names.insert(previous);
        }
        if !line.trim().is_empty() {
            previous = if line.starts_with(char::is_alphabetic) {
                line.trim()
            } else {
                ""
            };
        }
    }
    names
}

#[test]
fn every_appendix_production_has_exactly_one_row() {
    let spec = appendix_productions();
    let mut rows = BTreeSet::new();
    for row in ROWS {
        assert!(rows.insert(row.production), "{} twice", row.production);
    }
    let missing: Vec<_> = spec.difference(&rows).collect();
    let extra: Vec<_> = rows.difference(&spec).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "missing rows: {missing:?}\nrows the appendix lacks: {extra:?}"
    );
}

/// The sorted, deduplicated codes `src` parses to through `entry`, or why it
/// is not lossless.
fn codes(entry: Entry, src: &str) -> Result<Vec<&'static str>, String> {
    let tokens = tokenize(src);
    let parse = parse_entry(&tokens, src, entry);
    if parse.root.text() != src {
        return Err(format!("not lossless: {src:?}"));
    }
    let lexed = tokens.iter().filter_map(|t| t.error).map(|e| e.code());
    let mut got: Vec<&str> = parse.errors.iter().map(|e| e.code).chain(lexed).collect();
    got.sort_unstable();
    got.dedup();
    Ok(got)
}

#[test]
fn each_production_parses_its_positive_and_rejects_its_negative() {
    let mut failures = Vec::new();
    for row in ROWS {
        match codes(row.entry, row.positive) {
            Ok(got) if got.is_empty() => {}
            Ok(got) => failures.push(format!(
                "{}: positive {:?} gave {got:?}",
                row.production, row.positive
            )),
            Err(why) => failures.push(format!("{}: {why}", row.production)),
        }
        match codes(row.entry, row.negative) {
            Ok(got) if got == row.codes && !got.is_empty() => {}
            Ok(got) => failures.push(format!(
                "{}: negative {:?} expected {:?}, got {got:?}",
                row.production, row.negative, row.codes
            )),
            Err(why) => failures.push(format!("{}: {why}", row.production)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
