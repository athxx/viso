//! Templates and parts (§57, §58): a `template` parses to a typed declaration
//! and expands at each `use` into its caller's view, its parameters given by
//! the arguments, its slots by `fill`, and its parts overridden or replaced by
//! the caller; and the diagnostics of each (`E3601` for a member a template
//! cannot have and for a `use` that never ends).

use viso_dsl::ast::{AstNode, CompilationUnit, Item, ViewItem};
use viso_dsl::frontend::{Origin, compile_file};
use viso_dsl::ir::{UiItem, UiNode};
use viso_dsl::syntax::{Entry, SyntaxKind, SyntaxNode, parse_entry, tokenize};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

fn codes(source: &str) -> Vec<(String, String)> {
    compile_file(source, &origin())
        .errors()
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect()
}

fn has(found: &[(String, String)], code: &str, needle: &str) -> bool {
    found.iter().any(|(c, m)| c == code && m.contains(needle))
}

const CARD: &str = r#"
template TitledCard(title: String, size: F32 = 14.0) {
    slot content: OptionalSlot<Node>;
    const MARK: String = "!";
    fn shout(s: String) -> String { return s + MARK; }

    view {
        Column {
            part heading: Text { text: shout(title); }
            SlotOutlet { slot: content; }
        }
    }
}
"#;

#[test]
fn a_template_and_its_use_parse_to_typed_nodes() {
    let source = format!(
        "{CARD}\ncomponent C {{ view {{ Column {{ use TitledCard(\"a\") {{ \
         override part heading {{ color: #fff; }} replace part heading {{ Text {{}} }} \
         fill content {{ Text {{}} }} }}; }} }} }}"
    );
    let parse = parse_entry(&tokenize(&source), &source, Entry::CompilationUnit);
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    let root = SyntaxNode::new_root(parse.root);
    let cu = CompilationUnit::cast(root.clone()).expect("unit");
    let Some(Item::Template(t)) = cu.items().next() else {
        panic!("a template first")
    };
    assert_eq!(t.name().map(|n| n.text()), Some("TitledCard".into()));
    assert_eq!(t.params().len(), 2);
    let kinds: Vec<SyntaxKind> = root.descendants().iter().map(|n| n.kind()).collect();
    for kind in [
        SyntaxKind::TemplateUse,
        SyntaxKind::PartOverride,
        SyntaxKind::PartReplace,
        SyntaxKind::PartNode,
    ] {
        assert!(kinds.contains(&kind), "{kind:?}");
    }
    let uses = root
        .descendants()
        .into_iter()
        .filter_map(ViewItem::cast)
        .filter(|i| matches!(i, ViewItem::Use(_)))
        .count();
    assert_eq!(uses, 1);

    let unterminated =
        format!("{CARD}\ncomponent C {{ view {{ Column {{ use TitledCard(\"a\") }} }} }}");
    let parse = parse_entry(
        &tokenize(&unterminated),
        &unterminated,
        Entry::CompilationUnit,
    );
    assert!(!parse.errors.is_empty(), "a `use` ends with `;`");
}

/// The nodes of `items`, flattening nothing.
fn nodes(items: &[UiItem]) -> Vec<&UiNode> {
    items
        .iter()
        .filter_map(|i| match i {
            UiItem::Node(n) => Some(n),
            _ => None,
        })
        .collect()
}

#[test]
fn a_use_expands_its_template_with_the_callers_fills_overrides_and_replacements() {
    let source = format!(
        "{CARD}
export component Profile {{
    state name = \"Ada\";
    view {{
        Column {{
            use TitledCard(\"Profile\") {{
                override part heading {{ text: name; }}
                fill content {{ Text {{ text: \"body\"; }} }}
            }};
            use TitledCard(size: 12.0, title: \"Second\") {{
                replace part heading {{ Row {{}} }}
            }};
        }}
    }}
}}"
    );
    let compiled = compile_file(&source, &origin());
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let tree = &compiled.tree;
    assert_eq!(tree.instances.len(), 2, "one instance per `use`");
    assert_eq!(tree.instances[0].identity, "TitledCard#0");
    assert_eq!(tree.instances[1].args.len(), 2, "named arguments");
    let root = nodes(&tree.items)[0];
    let cards = nodes(&root.children);
    assert_eq!(cards.len(), 2);

    let first = nodes(&cards[0].children);
    assert_eq!(first.len(), 2, "the heading, then the filled body");
    let heading = first[0];
    assert_eq!(heading.local_name.as_deref(), Some("heading"));
    assert_eq!(heading.instance, 1, "the part is the template's node");
    let texts: Vec<_> = heading
        .pending
        .iter()
        .filter(|p| p.name == "text")
        .collect();
    assert_eq!(
        texts.len(),
        1,
        "the override replaces the template's own `text`"
    );
    assert_eq!(texts[0].instance, 0, "and is the caller's value");
    assert_eq!(first[1].instance, 0, "the fill is the caller's");

    let second = nodes(&cards[1].children);
    assert_eq!(second.len(), 1, "the replacement, and no fill");
    assert_eq!(second[0].type_name, "Row");
    assert_eq!(second[0].instance, 0, "the replacement is the caller's");
}

#[test]
fn a_template_holds_no_state_and_is_placed_by_use_only() {
    let tail = "\nexport component C { view { Column {} } }";
    let found = codes(&format!(
        "template T() {{ state n = 0; action go() {{}} input x: I64; view {{ Text {{}} }} }}{tail}"
    ));
    for what in ["a `state`", "an `action`", "an `input`"] {
        assert!(has(&found, "E3601", what), "{what}: {found:?}");
    }
    let found = codes(&format!("template T() {{ slot s: Slot<Node>; }}{tail}"));
    assert!(has(&found, "E3601", "has a `view`"), "{found:?}");

    let found = codes(&format!(
        "{CARD}\ncomponent K {{ view {{ Text {{}} }} }}\n\
         export component C {{ view {{ Column {{ TitledCard {{}} use K(); }} }} }}"
    ));
    assert!(
        has(&found, "E2103", "`TitledCard` is a template"),
        "{found:?}"
    );
    assert!(has(&found, "E2103", "`K` is no template"), "{found:?}");
}

#[test]
fn a_use_gives_each_parameter_once_with_a_value_of_its_type() {
    let at = |body: &str| {
        codes(&format!(
            "{CARD}\nexport component C {{ view {{ Column {{ {body} }} }} }}"
        ))
    };
    let found = at("use TitledCard();");
    assert!(has(&found, "E2103", "needs `title`"), "{found:?}");
    let found = at("use TitledCard(1);");
    assert!(found.iter().any(|(c, _)| c.starts_with("E21")), "{found:?}");
    let found = at("use TitledCard(\"a\", 1.0, 2);");
    assert!(has(&found, "E2103", "takes 2 arguments"), "{found:?}");
    let found = at("use TitledCard(\"a\", titel: \"b\");");
    assert!(has(&found, "E3101", "no parameter `titel`"), "{found:?}");
    let found = at("use TitledCard(\"a\", title: \"b\");");
    assert!(has(&found, "E3102", "already given"), "{found:?}");
    let found = at("use TitledCard(\"a\") { color: #fff; };");
    assert!(has(&found, "E3601", "holds only"), "{found:?}");
}

#[test]
fn an_override_or_replacement_names_a_part_of_the_placed_node() {
    let at = |body: &str| {
        codes(&format!(
            "{CARD}\nexport component C {{ view {{ Column {{ {body} }} }} }}"
        ))
    };
    let found = at("use TitledCard(\"a\") { override part head { color: #fff; } };");
    assert!(has(&found, "E3501", "no part `head`"), "{found:?}");
    let found = at("use TitledCard(\"a\") { override part heading { colour: #fff; } };");
    assert!(has(&found, "E3101", "no property `colour`"), "{found:?}");
    let found = at("Text { override part heading { color: #fff; } }");
    assert!(has(&found, "E2103", "is a widget"), "{found:?}");
    let found = at("use TitledCard(\"a\") { replace part heading { Text {} Text {} } };");
    assert!(has(&found, "E3502", "mounts one node, not 2"), "{found:?}");

    let found = codes(
        "component K { view { Column { part a: Text {} part a: Text {} } } }\n\
         export component C { view { Column { K {} } } }",
    );
    assert!(has(&found, "E2002", "the part `a`"), "{found:?}");

    // A component exposes its parts as a template does.
    let compiled = compile_file(
        "component K { view { Column { part a: Text {} } } }\n\
         export component C { state t = \"x\"; view { Column { K { override part a { text: t; } } } } }",
        &origin(),
    );
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
}

#[test]
fn a_use_that_places_its_own_template_is_e3601() {
    let found = codes(
        "template A() { view { Column { use B(); } } }\n\
         template B() { view { Column { use A(); } } }\n\
         template Leaf() { view { Text {} } }\n\
         template Twice() { view { Column { use Leaf(); use Leaf(); } } }\n\
         export component C { view { Column { use A(); use Twice(); } } }",
    );
    let recursive = found
        .iter()
        .filter(|(c, m)| c == "E3601" && m.contains("never ends"))
        .count();
    assert_eq!(
        recursive, 2,
        "each `use` on the cycle, not `Twice`: {found:?}"
    );
}

#[test]
fn a_member_const_is_a_constant_of_its_owner() {
    let compiled = compile_file(
        "export component C {\n\
             const STEP: I64 = 3;\n\
             state n = STEP;\n\
             fn twice() -> I64 { return STEP * 2; }\n\
             action go() { n = n + twice(); }\n\
             view { Text {} }\n\
         }",
        &origin(),
    );
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = std::rc::Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let component = module.component("C").expect("component");
    let go = module.layout(component).member("go").expect("go");
    let mut vm = viso_behavior::Vm::new(module, viso_behavior::Budget::default());
    let mut c = vm.instantiate(component, []).expect("instantiate");
    vm.call(&mut c, go, &[]).expect("go");
    assert_eq!(c.states(), &[viso_behavior::Value::Int(9)]);
}
