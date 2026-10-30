//! Slots: a component's view places `slot` (the default slot, `children`)
//! and `slot name`, optionally with default content; a caller's bare items
//! fill the default slot and `name { .. }` a named one. Default content is
//! the definer's, a fill the caller's, and a slot placed inside a fill
//! forwards the caller's own.

mod support;

use support::Rt;
use viso_dsl::ast::{AstNode, CompilationUnit, Item, ViewItem};
use viso_dsl::frontend::{Origin, compile_file};
use viso_dsl::ir::{UiItem, UiNode};
use viso_dsl::syntax::{Entry, SyntaxNode, parse_entry, tokenize};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    }
}

fn nodes(items: &[UiItem]) -> Vec<&UiNode> {
    items
        .iter()
        .filter_map(|i| match i {
            UiItem::Node(n) => Some(&**n),
            _ => None,
        })
        .collect()
}

const CARD: &str = r#"
component Card {
    input title: String
    view {
        Column {
            slot head { Text(title) }
            slot
            slot foot
        }
    }
}
"#;

#[test]
fn a_slot_and_a_fill_parse_to_typed_nodes() {
    let source =
        format!("{CARD}component C {{ view {{ Card {{ title: \"t\"; head {{ Row {{}} }} }} }} }}");
    let parsed = parse_entry(&tokenize(&source), &source, Entry::CompilationUnit);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let unit = CompilationUnit::cast(SyntaxNode::new_root(parsed.root)).expect("unit");
    let components: Vec<_> = unit
        .items()
        .filter_map(|i| match i {
            Item::Component(c) => Some(c),
            _ => None,
        })
        .collect();
    let view = components[0].view().expect("Card's view");
    let ViewItem::Anonymous(column) = view
        .block()
        .expect("a block")
        .items()
        .next()
        .expect("the column")
    else {
        panic!("an anonymous node");
    };
    let slots: Vec<_> = column
        .body()
        .expect("a body")
        .members()
        .filter_map(|m| match m {
            ViewItem::Slot(s) => Some(s),
            _ => None,
        })
        .collect();
    let names: Vec<_> = slots.iter().map(|s| s.slot_name()).collect();
    assert_eq!(names, ["head", "children", "foot"]);
    assert!(slots[0].body().is_some(), "default content");
    assert!(slots[1].body().is_none());

    let view = components[1].view().expect("C's view");
    let ViewItem::Anonymous(card) = view
        .block()
        .expect("a block")
        .items()
        .next()
        .expect("the card")
    else {
        panic!("an anonymous node");
    };
    let fill = card
        .body()
        .expect("a body")
        .members()
        .find_map(|m| match m {
            ViewItem::Fill(f) => Some(f),
            _ => None,
        })
        .expect("a fill");
    assert_eq!(fill.name().expect("a name").text(), "head");
}

#[test]
fn default_content_is_the_definers_and_a_fill_the_callers() {
    let source = format!(
        "{CARD}
export component Profile {{
    view {{
        Column {{
            Card {{ title: \"a\"; Text(\"body\") }}
            Card {{
                title: \"b\"
                head {{ Row {{}} }}
                foot {{ Text(\"x\"); Text(\"y\") }}
            }}
        }}
    }}
}}"
    );
    let compiled = compile_file(&source, &origin());
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let tree = &compiled.tree;
    let root = nodes(&tree.items)[0];
    let cards = nodes(&root.children);
    assert_eq!(cards.len(), 2);

    let first = nodes(&cards[0].children);
    let types: Vec<_> = first.iter().map(|n| n.type_name.as_str()).collect();
    assert_eq!(types, ["Text", "Text"], "the default head, then the body");
    assert_ne!(first[0].instance, 0, "the default head is the card's");
    assert_eq!(first[1].instance, 0, "the body is the caller's");

    let second = nodes(&cards[1].children);
    let types: Vec<_> = second.iter().map(|n| n.type_name.as_str()).collect();
    assert_eq!(types, ["Row", "Text", "Text"], "the fills replace the head");
    assert!(
        second.iter().all(|n| n.instance == 0),
        "the fills are the caller's"
    );
}

const FORWARD: &str = r#"
component Card {
    view { Column { slot head; slot } }
}
component Panel {
    view {
        Card {
            head { slot title { Text("untitled") { width: 20dp; height: 20dp } } }
            slot
        }
    }
}
export component App {
    state taps = 0
    view {
        Panel {
            title { Text("tap") { width: 20dp; height: 20dp; on click { taps += 1 } } }
            Text("body") { width: 20dp; height: 20dp; on click { taps += 10 } }
        }
    }
}
"#;

#[test]
fn a_slot_inside_a_fill_forwards_the_callers_nodes() {
    let compiled = compile_file(FORWARD, &origin());
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    for mut rt in [Rt::mount(FORWARD), Rt::packaged(FORWARD)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        rt.click(0);
        assert_eq!(rt.int("taps"), Some(1), "the app's title handler");
        rt.click(1);
        assert_eq!(rt.int("taps"), Some(11), "the app's body handler");
    }
}

fn codes(source: &str) -> Vec<&'static str> {
    compile_file(source, &origin())
        .errors()
        .map(|d| d.code)
        .collect()
}

#[test]
fn slot_conflicts_are_reported() {
    let place = |items: &str| {
        codes(&format!(
            "{CARD}export component C {{ view {{ Column {{ {items} }} }} }}"
        ))
    };
    assert_eq!(place("Card { title: \"t\"; hed { Row {} } }"), ["E3501"]);
    assert_eq!(
        place("Card { title: \"t\"; foot { Row {} }; foot { Row {} } }"),
        ["E3502"]
    );
    assert_eq!(
        place("Card { title: \"t\"; Row {}; children { Row {} } }"),
        ["E3502"]
    );
    assert_eq!(
        codes("component C { view { Column { slot a; Row { slot a } } } }"),
        ["E3502"]
    );
}
