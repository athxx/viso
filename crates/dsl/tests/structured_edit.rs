//! Structured edits (§143): each edit addresses its target by Symbol ID or
//! Syntax ID, lays its text out in place, and is refused when the edited
//! source has an error the source did not.

use viso_dsl::TextSize;
use viso_dsl::edit::{Created, Document, EditError, StructuredEdit, SyntaxId};
use viso_dsl::frontend::Origin;

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

const COUNTER: &str = "\
import viso::time;

export component Counter {
    input step: I64 = 1;
    state count = 0;

    task fetch(n: I64) -> Result<I64, String> {
        await time::sleep(1s);
        Ok(n)
    }

    action bump() {
        start fetch(count);
        count += step;
    }

    view {
        Column {
            width: 10dp;
            Text { text: \"a\"; }
        }
    }
}
";

fn doc(source: &str) -> Document {
    let doc = Document::new(source, &origin());
    let errors: Vec<_> = doc
        .diagnostics()
        .iter()
        .filter(|d| d.severity == viso_dsl::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    doc
}

fn apply(doc: &Document, edit: StructuredEdit) -> (String, Option<Created>) {
    let applied = doc.apply(&edit).unwrap_or_else(|e| panic!("{edit:?}: {e}"));
    (applied.source, applied.created)
}

/// The Syntax ID of the node whose source starts with `needle`.
fn node(doc: &Document, needle: &str) -> SyntaxId {
    let at = doc.source().find(needle).expect("needle") as u32;
    doc.syntax_id_at(TextSize::new(at)).expect("a view node")
}

#[test]
fn a_syntax_id_round_trips_through_its_text_and_survives_edits_elsewhere() {
    let d = doc(COUNTER);
    let text_node = node(&d, "Text {");
    let text = text_node.to_string();
    assert!(
        text.ends_with("/ViewDecl.0/AnonymousNode.0/AnonymousNode.0"),
        "{text}"
    );
    assert_eq!(text.parse::<SyntaxId>(), Ok(text_node.clone()));
    assert_eq!(
        d.range_of(&text_node).map(|r| &d.source()[r.as_usize()]),
        Some("Text { text: \"a\"; }")
    );

    // An edit above the node, in the same component, leaves its address valid.
    let counter = d.symbol(&["Counter"]).expect("Counter");
    let (source, _) = apply(
        &d,
        StructuredEdit::AddState {
            component: counter,
            name: "extra".into(),
            ty: None,
            init: "\"x\"".into(),
        },
    );
    let edited = doc(&source);
    assert_eq!(
        edited
            .range_of(&text_node)
            .map(|r| &edited.source()[r.as_usize()]),
        Some("Text { text: \"a\"; }")
    );
}

#[test]
fn members_go_in_canonical_order_at_their_siblings_indentation() {
    let d = doc(COUNTER);
    let counter = d.symbol(&["Counter"]).expect("Counter");
    let (source, created) = apply(
        &d,
        StructuredEdit::AddInput {
            component: counter,
            name: "label".into(),
            ty: "String".into(),
            default: Some("\"\"".into()),
        },
    );
    assert!(
        source
            .contains("    input step: I64 = 1;\n    input label: String = \"\";\n    state count"),
        "{source}"
    );
    let Some(Created::Symbol(label)) = created else {
        panic!("{created:?}")
    };
    assert_eq!(doc(&source).symbol(&["Counter", "label"]), Some(label));

    let (source, _) = apply(
        &d,
        StructuredEdit::AddState {
            component: counter,
            name: "total".into(),
            ty: Some("I64".into()),
            init: "0".into(),
        },
    );
    assert!(
        source.contains("    state count = 0;\n    state total: I64 = 0;\n"),
        "{source}"
    );

    let (source, _) = apply(
        &d,
        StructuredEdit::AddAction {
            component: counter,
            name: "reset".into(),
            params: String::new(),
            body: "count = 0;\ncount += step;".into(),
        },
    );
    assert!(
        source.contains(
            "        count += step;\n    }\n    action reset() {\n        count = 0;\n        count += step;\n    }\n\n    view {"
        ),
        "{source}"
    );
}

#[test]
fn a_checked_edit_is_refused_with_the_errors_it_introduces() {
    let d = doc(COUNTER);
    let counter = d.symbol(&["Counter"]).expect("Counter");
    let refused = d.apply(&StructuredEdit::AddState {
        component: counter,
        name: "count".into(),
        ty: None,
        init: "1".into(),
    });
    let Err(EditError::Rejected(errors)) = refused else {
        panic!("a second `count`: {refused:?}")
    };
    assert!(errors.iter().any(|e| e.code == "E2002"), "{errors:#?}");

    let refused = d.apply(&StructuredEdit::AddAction {
        component: counter,
        name: "oops".into(),
        params: String::new(),
        body: "missing = 1;".into(),
    });
    assert!(
        matches!(refused, Err(EditError::Rejected(_))),
        "{refused:?}"
    );

    // A stale symbol and a stale path are refused before any text is touched.
    let stale: viso_dsl::resolve::SymbolId = "0123456789abcdef0123456789abcdef".parse().unwrap();
    assert!(matches!(
        d.apply(&StructuredEdit::AddState {
            component: stale,
            name: "n".into(),
            ty: None,
            init: "1".into()
        }),
        Err(EditError::UnknownSymbol(_))
    ));
    let mut gone = node(&d, "Text {");
    gone.path.last_mut().unwrap().ordinal = 7;
    assert!(matches!(
        d.apply(&StructuredEdit::SetPropertyBinding {
            node: gone,
            property: "text".into(),
            value: "\"b\"".into()
        }),
        Err(EditError::UnknownSyntax(_))
    ));
    let state = d.symbol(&["Counter", "count"]).expect("count");
    assert!(matches!(
        d.apply(&StructuredEdit::AddState {
            component: state,
            name: "n".into(),
            ty: None,
            init: "1".into()
        }),
        Err(EditError::WrongTarget(_))
    ));
}

#[test]
fn view_edits_insert_bind_handle_and_wrap_nodes() {
    let d = doc(COUNTER);
    let column = node(&d, "Column {");
    let (source, created) = apply(
        &d,
        StructuredEdit::InsertNode {
            parent: column.clone(),
            index: 0,
            node: "Text {\n    text: \"first\";\n}".into(),
        },
    );
    assert!(
        source.contains(
            "            width: 10dp;\n            Text {\n                text: \"first\";\n            }\n            Text { text: \"a\"; }"
        ),
        "{source}"
    );
    // The new node's address chains a further edit onto it.
    let Some(Created::Syntax(first)) = created else {
        panic!("{created:?}")
    };
    let edited = doc(&source);
    let applied = edited
        .apply(&StructuredEdit::SetPropertyBinding {
            node: first,
            property: "text".into(),
            value: "\"changed\"".into(),
        })
        .expect("rebind");
    assert_eq!(applied.edits.len(), 1, "the value alone is replaced");
    assert!(applied.source.contains("text: \"changed\";"));

    let (source, _) = apply(
        &d,
        StructuredEdit::SetPropertyBinding {
            node: column.clone(),
            property: "height".into(),
            value: "20dp".into(),
        },
    );
    assert!(
        source.contains("            width: 10dp;\n            height: 20dp;\n"),
        "{source}"
    );

    let (source, created) = apply(
        &d,
        StructuredEdit::AttachEventHandler {
            node: column,
            event: "click".into(),
            pattern: None,
            body: "bump();".into(),
        },
    );
    assert!(
        source.contains(
            "            width: 10dp;\n            on click {\n                bump();\n            }\n            Text"
        ),
        "{source}"
    );
    assert!(matches!(created, Some(Created::Syntax(_))));

    let text = node(&d, "Text {");
    let (source, _) = apply(
        &d,
        StructuredEdit::WrapInKeyedFor {
            node: text,
            item: "n".into(),
            list: "[1, 2, 3]".into(),
            key: "n".into(),
        },
    );
    assert!(
        source.contains(
            "            for n in [1, 2, 3] key n {\n                Text { text: \"a\"; }\n            }"
        ),
        "{source}"
    );
}

#[test]
fn an_empty_body_opens_onto_its_own_lines() {
    let d = doc("export component C {\n    view {\n        Column {}\n    }\n}\n");
    let column = node(&d, "Column");
    let (source, _) = apply(
        &d,
        StructuredEdit::InsertNode {
            parent: column,
            index: 0,
            node: "Text {}".into(),
        },
    );
    assert!(
        source.contains("        Column {\n            Text {}\n        }\n"),
        "{source}"
    );
}

#[test]
fn a_task_returning_a_result_becomes_a_resource() {
    let d = doc(COUNTER);
    let fetch = d.symbol(&["Counter", "fetch"]).expect("fetch");
    let applied = d
        .apply(&StructuredEdit::ConvertTaskToResource {
            task: fetch,
            name: "loaded".into(),
            args: vec!["count".into()],
            key: "count".into(),
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let source = &applied.source;
    assert!(
        source.contains(
            "        Ok(n)\n    }\n    resource loaded: Resource<I64, String> {\n        load = fetch(count);\n        key = count;\n    }\n"
        ),
        "{source}"
    );
    assert!(
        source.contains("    action bump() {\n        count += step;\n    }"),
        "the bare start is gone: {source}"
    );
    assert_eq!(applied.edits.len(), 2);
    assert!(matches!(applied.created, Some(Created::Symbol(_))));

    let bump = d.symbol(&["Counter", "bump"]).expect("bump");
    assert!(matches!(
        d.apply(&StructuredEdit::ConvertTaskToResource {
            task: bump,
            name: "r".into(),
            args: Vec::new(),
            key: "0".into()
        }),
        Err(EditError::WrongTarget(_))
    ));
}

#[test]
fn file_level_edits_add_imports_components_and_impls() {
    let d = doc(COUNTER);
    let (source, _) = apply(
        &d,
        StructuredEdit::AddImport {
            path: "viso::math".into(),
        },
    );
    assert!(
        source.starts_with("import viso::time;\nimport viso::math;\n\n"),
        "{source}"
    );
    assert!(matches!(
        d.apply(&StructuredEdit::AddImport {
            path: "viso :: time".into()
        }),
        Err(EditError::Exists(_))
    ));

    let (source, created) = apply(
        &d,
        StructuredEdit::CreateComponent {
            name: "Badge".into(),
            export: false,
        },
    );
    assert!(
        source.ends_with("}\n\ncomponent Badge {\n    view {\n        Column {}\n    }\n}\n"),
        "{source}"
    );
    let Some(Created::Symbol(badge)) = created else {
        panic!("{created:?}")
    };
    assert_eq!(doc(&source).symbol(&["Badge"]), Some(badge));

    let shapes = "\
trait Area {
    fn area(self) -> F64;
}

record Square {
    side: F64;
}

export component C {
    view {
        Column {}
    }
}
";
    let d = doc(shapes);
    let (source, _) = apply(
        &d,
        StructuredEdit::AddTraitImpl {
            trait_name: "Area".into(),
            ty: "Square".into(),
            body: "fn area(self) -> F64 {\n    return self.side * self.side;\n}".into(),
        },
    );
    assert!(
        source.ends_with(
            "}\n\nimpl Area for Square {\n    fn area(self) -> F64 {\n        return self.side * self.side;\n    }\n}\n"
        ),
        "{source}"
    );
    // An impl missing the trait's method is refused.
    assert!(matches!(
        d.apply(&StructuredEdit::AddTraitImpl {
            trait_name: "Area".into(),
            ty: "Square".into(),
            body: String::new(),
        }),
        Err(EditError::Rejected(_))
    ));
}
