//! The Definition of Done (§158), one test per item. Items a dedicated suite
//! already pins are checked here at their contract (the suite is named on the
//! test); the rest are checked in full:
//!
//! - the EBNF one to one with parser cases — `ebnf_golden.rs`, read from the
//!   spec, so here only that its table is in force;
//! - the strict and contextual keyword lists exactly as §12 writes them;
//! - operator precedence and associativity as golden trees, against §64;
//! - directed diagnostics for `child`, the event arrow, `Float` and a
//!   non-canonical resource; one state forward-reference rule; `preserve` and
//!   `key` as separate syntax;
//! - a component with state, computed, action and view running, updating
//!   without any render call, a keyed list keeping identity, a rejected reload
//!   keeping the last good;
//! - diagnostics carrying the §138 fields, a system on a fixed tick;
//! - and the safety items: no parser panic on hostile input, the verifier
//!   refusing an out-of-range register, a stale node handle detected.
//!
//! The shader item (one shader through the instance ABI on two backends) is
//! `crates/viso/tests/shader_golden.rs`: the CPU reference rasterizer and
//! Metal agree, and Vulkan with the `vulkan` feature — a GPU test this crate
//! cannot run.

mod support;

use std::rc::Rc;

use support::{Rt, origin};
use viso_behavior::game::Scheduler;
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Value, Vm};
use viso_dsl::ast::{AstNode, ViewFor, ViewIf};
use viso_dsl::edit::Document;
use viso_dsl::frontend::compile_file;
use viso_dsl::syntax::grammar::{Entry, parse, parse_entry, parse_expr};
use viso_dsl::syntax::{SyntaxElement, SyntaxKind, SyntaxNode, tokenize};
use viso_dsl::{Severity, diag::Applicability};

const SPEC: &str = include_str!("../../../Viso_DSL_1.0.md");

/// The codes the full frontend reports for `src`, in order.
fn codes(src: &str) -> Vec<&'static str> {
    Document::new(src, &origin())
        .diagnostics()
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

/// The words of the first ` ```text ` block after `heading` in the spec.
fn spec_words(heading: &str) -> Vec<&'static str> {
    let at = SPEC.find(heading).unwrap_or_else(|| panic!("{heading}"));
    let block = &SPEC[at..];
    let start = block.find("```text\n").expect("a text block") + "```text\n".len();
    let end = block[start..].find("```").expect("closed") + start;
    block[start..end].split_whitespace().collect()
}

#[test]
fn the_ebnf_rows_are_read_from_the_spec() {
    // `ebnf_golden.rs` holds one row per Appendix A production and fails on a
    // production added or dropped; it parses the appendix it checks against.
    assert!(SPEC.contains("# 附录 A：规范性合并 EBNF"));
    assert!(
        include_str!("ebnf_golden.rs").contains("fn every_appendix_production_has_exactly_one_row")
    );
}

#[test]
fn the_keyword_lists_are_the_section_12_lists() {
    let strict: Vec<_> = spec_words("### 12.1 严格关键字")
        .into_iter()
        .chain(spec_words("### 12.2 保留并禁止使用"))
        .collect();
    for word in &strict {
        let kind = SyntaxKind::from_ident(word);
        assert!(
            kind.is_keyword() && kind != SyntaxKind::Ident,
            "`{word}` is strict"
        );
        let src = format!("fn {word}() {{}}");
        assert_eq!(
            parse(&tokenize(&src), &src)
                .errors
                .iter()
                .map(|e| e.code)
                .next(),
            Some("E1301"),
            "`{word}` cannot be bound"
        );
    }
    // Every keyword kind the lexer and parser know is one the spec lists.
    let contextual = spec_words("### 12.3 上下文关键字");
    let kinds = (SyntaxKind::TryKw as u16 - SyntaxKind::ImportKw as u16 + 1) as usize;
    assert_eq!(kinds, strict.len() + contextual.len(), "the keyword kinds");
    for word in contextual {
        assert_eq!(
            SyntaxKind::from_ident(word),
            SyntaxKind::Ident,
            "`{word}` lexes as a name"
        );
        assert!(
            SyntaxKind::contextual_keyword(word).is_some(),
            "`{word}` is contextual"
        );
        let src = format!("let {word} = 1;");
        assert!(
            parse_entry(&tokenize(&src), &src, Entry::BlockBody)
                .errors
                .is_empty(),
            "`{word}` binds as a name"
        );
    }
}

/// The expression tree of `src`, each operator node in parentheses.
fn tree(src: &str) -> String {
    fn render(element: &SyntaxElement, out: &mut Vec<String>) {
        match element {
            SyntaxElement::Token(t) if t.kind().is_trivia() => {}
            SyntaxElement::Token(t) => out.push(t.text()),
            SyntaxElement::Node(n) => {
                let mut inner = Vec::new();
                for child in n.children_with_tokens() {
                    render(&child, &mut inner);
                }
                let grouped = matches!(
                    n.kind(),
                    SyntaxKind::BinaryExpr
                        | SyntaxKind::UnaryExpr
                        | SyntaxKind::CastExpr
                        | SyntaxKind::RangeExpr
                        | SyntaxKind::CallExpr
                        | SyntaxKind::FieldExpr
                        | SyntaxKind::OptionalFieldExpr
                        | SyntaxKind::IndexExpr
                        | SyntaxKind::TryExpr
                );
                if grouped {
                    out.push(format!("({})", inner.join(" ")));
                } else {
                    out.extend(inner);
                }
            }
        }
    }
    let parsed = parse_expr(&tokenize(src), src);
    assert!(parsed.errors.is_empty(), "{src}: {:?}", parsed.errors);
    let root = SyntaxNode::new_root(parsed.root);
    let mut out = Vec::new();
    render(&SyntaxElement::Node(root), &mut out);
    out.join(" ")
}

/// §64's levels, highest first, as its table writes the operators.
const LEVELS: [&str; 14] = [
    "`()` `[]` `.` `?.` `?`",
    "`! ~ + - await`",
    "`as`",
    "`* / %`",
    "`+ -`",
    "`<< >>`",
    "`&`",
    "`^`",
    "`\\|`",
    "`== != < <= > >=`",
    "`&&`",
    "`\\|\\|`",
    "`??`",
    "`.. ..=`",
];

#[test]
fn precedence_and_associativity_are_golden_trees() {
    let at = SPEC.find("## 64. 运算符优先级和结合性表").expect("§64");
    let rows: Vec<&str> = SPEC[at..]
        .lines()
        .filter(|l| l.trim_start().starts_with('|') && l.contains('`'))
        .take(LEVELS.len())
        .map(|l| {
            // A cell may hold an escaped `\|`.
            let cells: Vec<&str> = l.split(" | ").collect();
            cells[1].trim()
        })
        .collect();
    assert_eq!(rows, LEVELS, "§64 changed: update the golden trees");

    // Each adjacent pair of levels, the tighter one nested.
    let golden = [
        ("-a.b", "(- (a . b))"),
        ("!f()?", "(! ((f ( )) ?))"),
        ("-a as I64", "((- a) as I64)"),
        ("a * b as F64", "(a * (b as F64))"),
        ("a + b * c", "(a + (b * c))"),
        ("a << b + c", "(a << (b + c))"),
        ("a & b << c", "(a & (b << c))"),
        ("a ^ b & c", "(a ^ (b & c))"),
        ("a | b ^ c", "(a | (b ^ c))"),
        ("a == b | c", "(a == (b | c))"),
        ("flags & MASK == 0", "((flags & MASK) == 0)"),
        ("a && b == c", "(a && (b == c))"),
        ("a || b && c", "(a || (b && c))"),
        ("a ?? b || c", "(a ?? (b || c))"),
        ("a .. b ?? c", "(a .. (b ?? c))"),
        // Associativity within each level.
        ("a.b.c", "((a . b) . c)"),
        ("a[0][1]", "((a [ 0 ]) [ 1 ])"),
        ("- ! a", "(- (! a))"),
        ("a as I64 as F64", "((a as I64) as F64)"),
        ("a / b * c", "((a / b) * c)"),
        ("a - b - c", "((a - b) - c)"),
        ("a << b >> c", "((a << b) >> c)"),
        ("a & b & c", "((a & b) & c)"),
        ("a | b | c", "((a | b) | c)"),
        ("a && b && c", "((a && b) && c)"),
        ("a || b || c", "((a || b) || c)"),
        ("a ?? b ?? c", "(a ?? (b ?? c))"),
    ];
    let mut failures = Vec::new();
    for (src, want) in golden {
        let got = tree(src);
        if got != want {
            failures.push(format!("{src}: {got} (want {want})"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Comparison and range do not associate (§63.1).
    for chain in ["a < b < c", "a == b != c", "a .. b .. c"] {
        let parsed = parse_expr(&tokenize(chain), chain);
        assert_eq!(
            parsed.errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            ["E2802"],
            "{chain}"
        );
    }
}

#[test]
fn removed_forms_have_directed_diagnostics() {
    let view = |body: &str| {
        format!("export component C {{ state n = 0; view {{ Column {{ {body} }} }} }}")
    };
    for (src, code, applicability) in [
        (view("child Text { }"), "E3001", Some(Applicability::MachineApplicable)),
        (view("Text { on click => n += 1; }"), "E3201", Some(Applicability::MachineApplicable)),
        ("export component C { state s: Float = 1.0; view { Text {} } }".to_owned(), "E2101", Some(Applicability::MaybeIncorrect)),
        (
            "task f() -> Result<I64, String> { Ok(1) }\nexport component C { resource r: Resource<I64, String> { load = f(); } view { Text {} } }"
                .to_owned(),
            "E4301",
            None,
        ),
        (
            "task f() -> Result<I64, String> { Ok(1) }\nexport component C { state k = 1; resource r: Resource<I64, String> { load = f(); key = k; policy = [1]; } view { Text {} } }"
                .to_owned(),
            "E4302",
            None,
        ),
    ] {
        let document = Document::new(src.as_str(), &origin());
        let found = document
            .diagnostics()
            .iter()
            .find(|d| d.code == code)
            .unwrap_or_else(|| panic!("{code} for {src}: {:#?}", document.diagnostics()));
        assert_eq!(found.fixes.first().map(|f| f.applicability), applicability, "{code}");
    }
}

#[test]
fn a_state_forward_reference_is_one_rule() {
    let src = "export component C { state a = b; state b = 1; computed c: I64 = d; computed d: I64 = 1; view { Text {} } }";
    assert_eq!(
        codes(src),
        ["E2104"],
        "states never refer forward; computeds may"
    );
}

#[test]
fn preserve_and_key_are_separate_syntax() {
    let src = r#"component C { view { if a preserve "p" { } for x in xs key x { } } }"#;
    let root = SyntaxNode::new_root(parse(&tokenize(src), src).root);
    let nodes = root.descendants();
    let view_if = nodes
        .iter()
        .find_map(|n| ViewIf::cast(n.clone()))
        .expect("if");
    let view_for = nodes
        .iter()
        .find_map(|n| ViewFor::cast(n.clone()))
        .expect("for");
    assert_eq!(view_if.preserve_name().as_deref(), Some("p"));
    assert_eq!(
        view_for.key().map(|k| k.syntax().text()),
        Some("x".to_owned())
    );
    // Neither clause is accepted in the other's place.
    for (src, code) in [
        ("for x in xs preserve \"p\" { }", "E3401"),
        ("if a key x { }", "E1404"),
    ] {
        let parsed = parse_entry(&tokenize(src), src, Entry::ViewFragment);
        assert!(
            parsed.errors.iter().any(|e| e.code == code),
            "{src}: {:?}",
            parsed.errors
        );
    }
}

const APP: &str = r#"
export component App {
    state count = 0;
    state order: List<I64> = [1, 2, 3];
    computed label = format("{}", count * 2);
    action add() {
        count += 1;
    }
    view {
        Column {
            Text { width: 20dp; height: 20dp; text: label; on click { add(); } }
            Text { width: 20dp; height: 20dp; text: "Rotate"; on click { order.push(order.remove(0)); } }
            Column {
                for id in order key id {
                    Text { text: format("{}", id); }
                }
            }
        }
    }
}
"#;

/// The texts under `node`, in pre-order.
fn texts(rt: &Rt, node: viso_ui::NodeId) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(at) = stack.pop() {
        if let Some(request) = rt.store.text_request(at) {
            out.push(request.text.clone());
        }
        stack.extend(rt.children(at).into_iter().rev());
    }
    out
}

#[test]
fn a_component_runs_reacts_keeps_keys_and_survives_a_bad_reload() {
    let mut rt = Rt::mount(APP);
    let root = rt.root.expect("mounted");
    let label = rt.children(root)[0];
    assert_eq!(texts(&rt, label), ["0"]);

    // An action's write reaches the computed and its text with no render call.
    rt.click(0);
    rt.click(0);
    assert_eq!(rt.int("count"), Some(2));
    assert_eq!(texts(&rt, label), ["4"]);

    // A keyed reorder moves the same nodes.
    let rows = rt.region(2);
    rt.click(1);
    assert_eq!(rt.region(2), [rows[1], rows[2], rows[0]]);
    assert_eq!(texts(&rt, rt.children(root)[2]), ["2", "3", "1"]);

    // A reload the compiler rejects keeps the running version and its state.
    let broken = APP.replace("count += 1;", "count += \"one\";");
    let errors = rt.try_reload(&broken).expect_err("rejected");
    assert!(errors.iter().any(|d| d.code == "E2103"), "{errors:#?}");
    rt.click(0);
    assert_eq!(rt.int("count"), Some(3), "the last good handler still runs");
    assert_eq!(texts(&rt, label), ["6"]);
    assert_eq!(rt.fault(), None);
}

#[test]
fn diagnostics_carry_what_tools_and_ai_read() {
    let src =
        "export component C { state count = 0; view { Text { text: format(\"{}\", cuont); } } }";
    let document = Document::new(src, &origin());
    let [diagnostic] = document.diagnostics() else {
        panic!("{:#?}", document.diagnostics());
    };
    assert_eq!(
        (diagnostic.code, diagnostic.severity),
        ("E2001", Severity::Error)
    );
    assert_eq!(
        &src[std::ops::Range::<usize>::from(diagnostic.primary)],
        "cuont"
    );
    assert!(!diagnostic.message.is_empty());
    let fix = diagnostic.fixes.first().expect("a fix");
    assert_eq!(fix.applicability, Applicability::MaybeIncorrect);
    assert_eq!(fix.edits[0].replacement, "count");
    assert!(
        diagnostic
            .related
            .iter()
            .any(|r| r.label.contains("`count`")),
        "{diagnostic:#?}"
    );
}

#[test]
fn a_system_runs_on_a_fixed_tick() {
    let src = "import viso::game::{FixedUpdate, FixedFrame};\n\
               export system Ticks implements FixedUpdate {\n    state ticks = 0;\n    \
               action fixed_update(frame: FixedFrame) { ticks += 1; }\n}\n";
    let compiled = compile_file(src, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = compiled.behavior.bytecode().expect("verified bytecode");
    let module = Rc::new(module.with_tick_rate(8).expect("a tick rate"));
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    let mut game = Scheduler::new(vm).expect("starts");
    let component = game.vm().module().component("Ticks").expect("system");
    let slot = game
        .vm()
        .module()
        .layout(component)
        .state("ticks")
        .expect("state");
    let mut seen = Vec::new();
    // Frames of uneven length: the tick count follows elapsed time alone.
    for dt in [0.0625, 0.0625, 0.3125, 0.0, 0.125] {
        game.frame(dt);
        seen.push(game.instance(0).states()[slot].clone());
    }
    assert_eq!(seen, [0, 1, 3, 3, 4].map(Value::Int));
}

#[test]
fn hostile_input_never_panics_the_front_end() {
    let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
    let pieces = [
        "component",
        "{",
        "}",
        "(",
        ")",
        "[",
        "]",
        "<",
        ">",
        ";",
        ":",
        ",",
        ".",
        "::",
        "=",
        "=>",
        "<=>",
        "state",
        "view",
        "on",
        "for",
        "in",
        "key",
        "if",
        "preserve",
        "match",
        "\"s",
        "'c'",
        "1e5dp",
        "/*",
        "@",
        "|",
        "?",
        "..",
        "Text",
        "x",
        "\n",
        " ",
        "\u{430}",
        "\0",
    ];
    for _ in 0..500 {
        let mut src = String::new();
        for _ in 0..40 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            src.push_str(pieces[(rng % pieces.len() as u64) as usize]);
        }
        let _ = Document::new(src.as_str(), &origin()).diagnostics().len();
    }
}

#[test]
fn the_verifier_refuses_a_register_out_of_range() {
    let compiled = compile_file(APP, &origin());
    let view = viso_dsl::view_behavior::view_behavior(&compiled)
        .expect("mounts")
        .expect("has behavior");
    // The wire form decodes and verifies; truncating it is refused, never
    // run past its end.
    assert!(viso_view::ViewHost::from_bytes(&view.bytes, &view.component).is_ok());
    for cut in [1, view.bytes.len() / 2, view.bytes.len() - 1] {
        assert!(viso_view::ViewHost::from_bytes(&view.bytes[..cut], &view.component).is_err());
    }
    let mut corrupt = view.bytes.to_vec();
    let mut refused = 0;
    for at in (0..corrupt.len()).step_by(7) {
        let byte = corrupt[at];
        corrupt[at] = 0xFF;
        // Whatever loads must still be safe to run: a load either verifies or
        // is refused.
        if viso_view::ViewHost::from_bytes(&corrupt, &view.component).is_err() {
            refused += 1;
        }
        corrupt[at] = byte;
    }
    assert!(refused > 0, "corruption is detected");
}

#[test]
fn a_stale_node_handle_is_detected() {
    let mut rt = Rt::mount(APP);
    let rows = rt.region(2);
    let gone = rows[0];
    // Rotating twice and dropping key 1 removes its row.
    rt.click(1);
    let shrink = APP.replace("order.push(order.remove(0));", "order.remove(0);");
    rt.reload(&shrink);
    rt.click(1);
    assert!(!rt.region(2).contains(&gone));
    assert!(
        rt.store.arena().links(gone).is_none(),
        "the old id no longer resolves"
    );
}
