//! Reactive property values reaching their nodes: a label's text, a text
//! field's seeded buffer and a control's displayed value, evaluated from the
//! view's handler table at mount and re-evaluated when a state they read
//! changes.

use std::rc::Rc;

use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::plan_view;
use viso_dsl::ir::binding_ir::NodeKey;
use viso_ui::StateStore;
use viso_view::{ControlKind, Scope, Value, ViewHost};

const SOURCE: &str = r#"
export component Values {
    state count = 2;
    state name = "Ada";
    view {
        Column {
            Text { text: format("{} items", count); }
            Button { text: "Go"; }
            TextInput { bind value <=> name; }
            Text { }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["values".into()],
        language: None,
    }
}

#[test]
fn a_label_text_and_a_field_value_compile_to_value_entries() {
    let plan = plan_view(SOURCE, &origin()).expect("the view compiles");
    let view = plan.view.expect("the view has behavior");
    let mut host = ViewHost::new(Rc::clone(&view.module), &view.component).expect("mounts");
    let states = StateStore::default();
    let mut text = |key: u32, kind: ControlKind| {
        let control = view.control(NodeKey(key)).expect("a view-driven node");
        assert_eq!(control.kind, kind);
        let entry = control.value.expect("a value entry");
        match host.evaluate(entry, &Scope::EMPTY, None, &states) {
            Ok(Value::Str(text)) => text.as_str().to_owned(),
            other => panic!("the entry evaluates to a text, not {other:?}"),
        }
    };
    assert_eq!(text(1, ControlKind::Label), "2 items");
    assert_eq!(text(2, ControlKind::Label), "Go");
    assert_eq!(text(3, ControlKind::TextInput), "Ada");
    assert_eq!(view.control(NodeKey(4)), None, "a label without a text");
}
